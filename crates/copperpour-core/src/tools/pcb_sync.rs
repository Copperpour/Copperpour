//! Pure schematic-to-board synchronization planning.
//!
//! The public tool handler and KiCad IPC adapter live outside this module.
//! This module owns the deep planning interface: turn a KiCad-exported
//! flattened netlist plus a board snapshot into a complete, immutable plan.

use std::borrow::Cow;
use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::path::{Path, PathBuf};

use super::library::PadFabricationProperty;
use super::library_footprint::{
    carries_kicad_10_0_7_fields, carries_net_ties, reported_fab_property, CarriedByKiCad,
};
use crate::hex::encode_hex;
use crate::mcp::protocol::{CallToolResult, ToolContent};
use crate::tools::{
    pcb_board::{attempt_ipc_write, BoardWrite},
    BoundedList, ToolContext,
};
use anyhow::{bail, Context, Result};
use copperpour_sexp::paths::display_path;
use copperpour_sexp::SexpNode;
use prost::Message;
use serde::Serialize;
use sha2::{Digest, Sha256};

// Count bound only: this does not claim a byte-size or listener-size limit.
const SYNC_CREATE_CHUNK_SIZE: usize = 32;

fn create_sync_items_in(
    client: &copperpour_ipc::KiCadIpcClient,
    document: &copperpour_ipc::gen::kiapi::common::types::DocumentSpecifier,
    creates: &[prost_types::Any],
) -> Result<()> {
    for chunk in creates.chunks(SYNC_CREATE_CHUNK_SIZE) {
        client.create_items_in(document.clone(), chunk.to_vec())?;
    }
    Ok(())
}

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

/// The sheet a symbol is placed on and its library symbol's footprint
/// filters, which KiCad's Update PCB sets on the footprint together. Rule
/// areas generated from sheets, and Repeat Layout with them, find a sheet's
/// footprints by `sheet_name` (#237).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
struct SymbolSheet {
    sheet_name: String,
    sheet_file: String,
    footprint_filters: String,
}

impl SymbolSheet {
    fn of(footprint: &copperpour_ipc::gen::kiapi::board::types::FootprintInstance) -> Self {
        Self {
            sheet_name: footprint.symbol_sheet_name.clone(),
            sheet_file: footprint.symbol_sheet_filename.clone(),
            footprint_filters: footprint.symbol_footprint_filters.clone(),
        }
    }

    fn listed(&self) -> String {
        format!(
            "sheet name {:?}, sheet file {:?}, footprint filters {:?}",
            self.sheet_name, self.sheet_file, self.footprint_filters
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
        /// The footprint differs from its symbol in its sheet alone (#237).
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
            let mut plan = plan_sync_acknowledging(
                &netlist_source,
                &design,
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
                let kicad = client.get_kicad_version()?;
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
                let kicad = client.get_kicad_version()?;
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

/// Normalize a schematic symbol path for identity comparison by stripping at
/// most one leading `/<root_sheet_uuid>` segment when it matches the requested
/// root schematic's UUID and is followed by at least one non-empty segment
/// (#82).
///
/// KiCad's netlist exporter omits the root sheet UUID
/// (`SCH_SHEET_PATH::PathAsString`), while saved schematic instance paths and
/// some PCB footprints carry it. Foreign root UUIDs, other sheet prefixes, and
/// bare `/<root>` paths are returned unchanged so they remain distinct
/// identities.
fn normalize_symbol_path<'a>(path: &'a str, root_sheet_uuid: Option<&str>) -> &'a str {
    let Some(root_uuid) = root_sheet_uuid.filter(|uuid| !uuid.is_empty() && !uuid.contains('/'))
    else {
        return path;
    };
    let Some(without_leading_slash) = path.strip_prefix('/') else {
        return path;
    };
    let Some(rest) = without_leading_slash.strip_prefix(root_uuid) else {
        return path;
    };
    let Some(tail) = rest.strip_prefix('/') else {
        return path;
    };
    if tail.is_empty() || tail.starts_with('/') {
        return path;
    }
    rest
}

/// [`plan_sync_acknowledging`] with no routed pad net change acknowledged.
#[cfg(test)]
fn plan_sync(netlist_source: &str, design: &ExportedDesign, board: &BoardState) -> SyncPlan {
    plan_sync_acknowledging(netlist_source, design, board, &BTreeSet::new(), false)
}

/// The plan. A pad net change on a net that routed copper uses is a
/// `routed_pad_net_change` conflict unless `acknowledged` names it exactly;
/// then it is planned as KiCad's Update PCB makes it and noted; the vias and
/// zones of a net that leaves the schematic follow (`plan_copper_nets`). An acknowledgement the plan has no such change for is a
/// conflict, so a stale or widened one refuses (#38). With
/// `delete_without_symbols`, a plan with no conflict deletes each unlocked
/// footprint no symbol matched that lacks the board-only attribute (#861);
/// the footprint of a symbol marked Exclude from board is one of them (#870).
fn plan_sync_acknowledging(
    netlist_source: &str,
    design: &ExportedDesign,
    board: &BoardState,
    acknowledged: &BTreeSet<AcknowledgedNetChange>,
    delete_without_symbols: bool,
) -> SyncPlan {
    let mut diagnostics = Vec::new();
    let mut acknowledgements_used = BTreeSet::new();
    // Reported beside the plan without refusing it, as KiCad's Update PCB
    // reports a warning.
    let mut notes = Vec::new();
    // Parts skipped rather than refusing the plan (`SyncPlan::refused_updates`).
    let mut refused_updates = Vec::new();
    let mut counts = SyncCounts::default();
    let mut changes = Vec::new();
    let mut board_by_path = HashMap::new();
    let mut board_by_reference = HashMap::new();

    // Every reference the schematic export names, on either side of the
    // `on_board` flag. A duplicate board reference only matters when this set
    // contains it: `board_by_reference` is consulted at four sites below, and
    // each one looks up a reference that came from the export —
    // `skipped.reference` once and `component.reference` three times. A
    // reference the export never names is therefore never looked up, so which
    // of the duplicates `insert` happened to keep is unobservable.
    //
    // The map is still built from every footprint, duplicates included. Only
    // the diagnostic is scoped; suppressing the insert would change which
    // footprint an unrelated adoption resolves to.
    let exported_references = design
        .components
        .iter()
        .map(|component| component.reference.as_str())
        .chain(
            design
                .skipped
                .iter()
                .map(|skipped| skipped.reference.as_str()),
        )
        .chain(
            design
                .unassigned
                .iter()
                .map(|unassigned| unassigned.reference.as_str()),
        )
        .collect::<HashSet<_>>();

    let root_uuid = design.root_sheet_uuid.as_deref();
    for (index, footprint) in board.footprints.iter().enumerate() {
        let duplicate_reference = board_by_reference
            .insert(footprint.reference.as_str(), index)
            .is_some();
        if duplicate_reference && exported_references.contains(footprint.reference.as_str()) {
            diagnostics.push(conflict(
                "duplicate_board_reference",
                format!("board contains duplicate reference {}", footprint.reference),
                Some(&footprint.reference),
            ));
        }
        if let Some(path) = footprint.symbol_path.as_deref() {
            let normalized = normalize_symbol_path(path, root_uuid);
            if board_by_path.insert(normalized, index).is_some() {
                diagnostics.push(conflict(
                    "duplicate_board_identity",
                    format!("board contains duplicate schematic identity {normalized}"),
                    Some(&footprint.reference),
                ));
            }
        }
    }

    let mut matched = std::collections::HashSet::new();
    let mut design_references = std::collections::HashSet::new();
    let mut design_paths = std::collections::HashSet::new();
    let staging_x = board.bounds.max_x + 10.0;
    let mut add_index = 0usize;

    let mut skipped_references = HashSet::new();
    let mut skipped_paths = HashSet::new();
    // The footprints of `on_board=no` instances. KiCad's Update PCB skips a
    // symbol marked Exclude from board, and its pass over unmatched
    // footprints counts no match for that symbol's footprint, so it is one
    // with no symbol: kept, or deleted with "Delete footprints with no
    // symbols" (`BOARD_NETLIST_UPDATER::UpdateNetlist`,
    // board_netlist_updater.cpp:2092 and :2350, KiCad 10.0.8; #870). They
    // stay out of `matched` for that pass below, and out of the rename
    // candidates, since their symbol still claims them.
    let mut excluded = HashSet::new();
    for skipped in &design.skipped {
        let skipped_path = normalize_symbol_path(&skipped.symbol_path, root_uuid);
        if !skipped_references.insert(skipped.reference.as_str())
            || !skipped_paths.insert(skipped_path)
        {
            diagnostics.push(conflict(
                "duplicate_skipped_identity",
                format!(
                    "on_board=no instance {} has a duplicate reference or identity",
                    skipped.reference
                ),
                Some(&skipped.reference),
            ));
            continue;
        }
        counts.skipped_by_flag.planned += 1;
        // Matched as a component is: by identity, or by reference when the
        // footprint has none.
        let existing = board_by_path.get(skipped_path).copied().or_else(|| {
            board_by_reference
                .get(skipped.reference.as_str())
                .copied()
                .filter(|index| board.footprints[*index].symbol_path.is_none())
        });
        if let Some(index) = existing {
            excluded.insert(index);
        }
    }

    // No footprint assigned: nothing can be placed, so the part is reported
    // with what the board holds for it and the sync goes on for everything
    // else — eeschema's own Update PCB behaviour. Never a diagnostic: a
    // diagnostic clears the whole plan, which is exactly what #507 fixes.
    let mut unassigned = Vec::new();
    for component in &design.unassigned {
        counts.unassigned_footprint.planned += 1;
        let unassigned_path = normalize_symbol_path(&component.symbol_path, root_uuid);
        let existing = board_by_path.get(unassigned_path).copied().or_else(|| {
            board_by_reference
                .get(component.reference.as_str())
                .copied()
        });
        let board_state = match existing {
            Some(index) => {
                // Left untouched, as KiCad leaves it; counted as matched so
                // it is not reported as a board-only footprint.
                matched.insert(index);
                UnassignedBoardState::Kept
            }
            None => UnassignedBoardState::Absent,
        };
        unassigned.push(UnassignedFootprint {
            reference: component.reference.clone(),
            value: component.value.clone(),
            lib_id: component.lib_id.clone(),
            symbol_path: component.symbol_path.clone(),
            board_state,
        });
    }

    for component in &design.components {
        let normalized_path = normalize_symbol_path(&component.symbol_path, root_uuid);
        if !design_references.insert(component.reference.as_str()) {
            diagnostics.push(conflict(
                "duplicate_schematic_reference",
                format!(
                    "schematic export contains duplicate reference {}",
                    component.reference
                ),
                Some(&component.reference),
            ));
            continue;
        }
        if !design_paths.insert(normalized_path) {
            diagnostics.push(conflict(
                "duplicate_schematic_identity",
                format!(
                    "schematic export contains duplicate identity {}",
                    component.symbol_path
                ),
                Some(&component.reference),
            ));
            continue;
        }

        // A footprint linked to any unit of the symbol is its footprint, as
        // KiCad's Update PCB matches it against the sheet path and each of
        // the component's unit UUIDs (`BOARD_NETLIST_UPDATER::UpdateNetlist`,
        // KiCad 10.0.8). KiCad then relinks it to the first unit; the sync
        // keeps its link, as it keeps every matched footprint's.
        let unit_matches = component
            .unit_paths()
            .filter_map(|path| {
                board_by_path
                    .get(normalize_symbol_path(path, root_uuid))
                    .copied()
            })
            .collect::<BTreeSet<_>>();
        if unit_matches.len() > 1 {
            matched.extend(unit_matches.iter().copied());
            diagnostics.push(conflict(
                "duplicate_board_identity",
                format!(
                    "board footprints {} are each linked to a unit of {}",
                    unit_matches
                        .iter()
                        .map(|index| board.footprints[*index].reference.as_str())
                        .collect::<Vec<_>>()
                        .join(", "),
                    component.reference
                ),
                Some(&component.reference),
            ));
            continue;
        }
        let matched_index = unit_matches.first().copied().or_else(|| {
            board_by_reference
                .get(component.reference.as_str())
                .copied()
                .filter(|index| board.footprints[*index].symbol_path.is_none())
        });

        let Some(index) = matched_index else {
            if let Some(index) = board_by_reference
                .get(component.reference.as_str())
                .copied()
            {
                diagnostics.push(conflict(
                    "reference_identity_conflict",
                    format!(
                        "reference {} belongs to a different schematic identity on the board",
                        component.reference
                    ),
                    Some(&board.footprints[index].reference),
                ));
                continue;
            }
            let possible_renames = board
                .footprints
                .iter()
                .enumerate()
                .filter(|(index, footprint)| {
                    !matched.contains(index)
                        && !excluded.contains(index)
                        && footprint.symbol_path.is_none()
                        && !footprint.not_in_schematic
                        && footprint.footprint_id == component.footprint_id
                        && footprint.value == component.value
                })
                .collect::<Vec<_>>();
            if !possible_renames.is_empty() {
                diagnostics.push(conflict(
                    "reference_only_rename_ambiguous",
                    format!(
                        "{} has no stable board identity and could be a rename of {}; link or resolve the identity in KiCad",
                        component.reference,
                        possible_renames
                            .iter()
                            .map(|(_, footprint)| footprint.reference.as_str())
                            .collect::<Vec<_>>()
                            .join(", ")
                    ),
                    Some(&component.reference),
                ));
                continue;
            }
            let position = Point {
                x: staging_x,
                y: board.bounds.min_y + add_index as f64 * 10.0,
            };
            add_index += 1;
            changes.push(PlannedChange::Add {
                reference: component.reference.clone(),
                value: component.value.clone(),
                footprint_id: component.footprint_id.clone(),
                symbol_path: component.symbol_path.clone(),
                dnp: component.dnp,
                exclude_from_bom: component.exclude_from_bom,
                pad_nets: component.pad_nets.clone(),
                unconnected_pins: component.unconnected_pins.clone(),
                position,
                fields: component.fields.clone(),
                sheet: component.sheet.clone(),
            });
            counts.added.planned += 1;
            continue;
        };

        matched.insert(index);
        let footprint = &board.footprints[index];
        if footprint.reference != component.reference {
            if let Some(other_index) = board_by_reference
                .get(component.reference.as_str())
                .copied()
                .filter(|other_index| *other_index != index)
            {
                diagnostics.push(conflict(
                    "reference_rename_collision",
                    format!(
                        "cannot rename {} to {} because that reference belongs to board footprint {}",
                        footprint.reference,
                        component.reference,
                        board.footprints[other_index].kiid
                    ),
                    Some(&component.reference),
                ));
                continue;
            }
        }
        // A different footprint in the schematic is replaced, as KiCad's
        // Update PCB replaces it (`BOARD_NETLIST_UPDATER::replaceFootprint`).
        // Whether the new footprint can be built, and has every pad the
        // schematic connects, is checked against its library in the
        // handler's preflight (`prepare_replacements`). KiCad leaves a locked
        // footprint as it is, with a warning, and still updates the rest of
        // it; so does this.
        let replace_footprint = if footprint.footprint_id == component.footprint_id {
            None
        } else if footprint.locked {
            notes.push(conflict(
                "footprint_locked_not_replaced",
                format!(
                    "{} is locked, so it keeps {} though the schematic names {}; unlock it and \
                     sync again to replace it",
                    component.reference, footprint.footprint_id, component.footprint_id
                ),
                Some(&component.reference),
            ));
            None
        } else {
            Some(ReplacedFootprint {
                from: footprint.footprint_id.clone(),
                to: component.footprint_id.clone(),
            })
        };
        // A pin KiCad left unconnected needs no pad on the footprint kept;
        // a replacement is given its pads in the handler, from its library
        // footprint (`leave_out_unconnected_pins`).
        let pad_nets = if replace_footprint.is_none() {
            nets_for_pads(
                &component.pad_nets,
                &component.unconnected_pins,
                &footprint.pad_numbers,
            )
        } else {
            component.pad_nets.clone()
        };
        // A pad the schematic connects and the live footprint lacks used to
        // reach `apply_footprint_fields` and fail the apply, after a dry run
        // that said `ready` (#657). The part is skipped instead, as an added
        // part is that lacks one, and the rest is still synced. A replacement
        // is checked against its new footprint.
        let missing = missing_pads(&pad_nets, &footprint.pad_numbers);
        if replace_footprint.is_none() && !missing.is_empty() {
            refused_updates.push(pad_missing_conflict(
                &component.reference,
                &footprint.footprint_id,
                &missing,
            ));
            continue;
        }

        let mut changed_pads = 0usize;
        let pad_numbers = pad_nets
            .keys()
            .chain(footprint.pad_nets.keys())
            .collect::<std::collections::BTreeSet<_>>();
        for number in pad_numbers {
            let new_net = pad_nets.get(number).map(String::as_str).unwrap_or("");
            let old_net = footprint
                .pad_nets
                .get(number)
                .map(String::as_str)
                .unwrap_or("");
            if old_net == new_net {
                continue;
            }
            if board.routed_nets.contains_key(old_net) || board.routed_nets.contains_key(new_net) {
                let change = NetChange {
                    pad: number.clone(),
                    from: old_net.to_string(),
                    to: new_net.to_string(),
                };
                let acknowledgement = AcknowledgedNetChange {
                    reference: component.reference.clone(),
                    pad: change.pad.clone(),
                    from: change.from.clone(),
                    to: change.to.clone(),
                };
                if acknowledged.contains(&acknowledgement) {
                    changed_pads += 1;
                    let mut note = conflict(
                        "routed_pad_net_change_acknowledged",
                        format!(
                            "{} pad {} changes from '{}' to '{}' as acknowledged; KiCad's commit \
                             carries the tracks joined to it, the vias and zones of a net that \
                             leaves the schematic are moved as copper_net_changes, and the apply \
                             reports the routed copper on both nets and runs DRC",
                            component.reference, number, old_net, new_net
                        ),
                        Some(&component.reference),
                    );
                    note.net_change = Some(change);
                    notes.push(note);
                    acknowledgements_used.insert(acknowledgement);
                } else {
                    let mut diagnostic = conflict(
                        "routed_pad_net_change",
                        format!(
                            "{} pad {} would change from '{}' to '{}' while routed copper uses that net",
                            component.reference, number, old_net, new_net
                        ),
                        Some(&component.reference),
                    );
                    diagnostic.net_change = Some(change);
                    diagnostics.push(diagnostic);
                }
            } else {
                changed_pads += 1;
            }
        }

        // KiCad's Update PCB updates a footprint whose fields differ from the
        // symbol's and leaves fields the symbol lacks in place (#788). A
        // replacement takes the new footprint's fields, so it gets every one
        // of the symbol's again.
        let changed_fields = component
            .fields
            .iter()
            .filter(|field| {
                replace_footprint.is_some()
                    || footprint.fields.get(&field.name) != Some(&field.value)
            })
            .cloned()
            .collect::<Vec<_>>();
        let preserved_symbol_path = footprint
            .symbol_path
            .as_deref()
            .unwrap_or(component.symbol_path.as_str());
        let differs_beyond_sheet = replace_footprint.is_some()
            || footprint.reference != component.reference
            || footprint.value != component.value
            || footprint.symbol_path.is_none()
            || footprint.dnp != component.dnp
            || footprint.exclude_from_bom != component.exclude_from_bom
            || changed_pads > 0
            || !changed_fields.is_empty();
        if differs_beyond_sheet || footprint.sheet != component.sheet {
            changes.push(PlannedChange::Update {
                kiid: footprint.kiid.clone(),
                reference: component.reference.clone(),
                value: component.value.clone(),
                symbol_path: preserved_symbol_path.to_string(),
                dnp: component.dnp,
                exclude_from_bom: component.exclude_from_bom,
                pad_nets,
                unconnected_pins: component.unconnected_pins.clone(),
                preserve: PreservedBoardState {
                    position: footprint.position,
                    rotation: footprint.rotation,
                    layer: footprint.layer.clone(),
                    locked: footprint.locked,
                },
                fields: changed_fields,
                sheet: component.sheet.clone(),
                sheet_only: !differs_beyond_sheet,
                replace_footprint,
            });
            counts.updated.planned += 1;
            counts.pads_reassigned.planned += changed_pads;
        }
    }

    for unused in acknowledged.difference(&acknowledgements_used) {
        diagnostics.push(conflict(
            "net_change_acknowledgement_unmatched",
            format!(
                "{} pad {} from '{}' to '{}' is acknowledged, but this plan makes no such routed \
                 pad net change; dry run again and acknowledge what it reports",
                unused.reference, unused.pad, unused.from, unused.to
            ),
            Some(&unused.reference),
        ));
    }

    counts.conflicts.planned = diagnostics.len();
    if !diagnostics.is_empty() {
        changes.clear();
        counts.added.planned = 0;
        counts.updated.planned = 0;
        counts.pads_reassigned.planned = 0;
    }
    // The footprints no symbol matched. KiCad's Update PCB with "Delete
    // footprints with no symbols" removes each of them in the same commit,
    // never one with the board-only attribute (`FP_BOARD_ONLY`), and a locked
    // one only when locks are overridden, which the sync does not offer: it
    // keeps it with a warning (`BOARD_NETLIST_UPDATER::UpdateNetlist`, KiCad
    // 10.0.8). A plan with a conflict deletes nothing (#861). The footprint of
    // a symbol marked Exclude from board is handled the same way (#870).
    let deleting = delete_without_symbols && diagnostics.is_empty();
    let mut without_symbols = Vec::new();
    let mut unmatched = 0usize;
    for (index, footprint) in board.footprints.iter().enumerate() {
        if matched.contains(&index) {
            continue;
        }
        unmatched += 1;
        if footprint.not_in_schematic {
            continue;
        }
        let reason = if excluded.contains(&index) {
            WithoutSymbolReason::ExcludedFromBoard
        } else {
            WithoutSymbolReason::SymbolRemoved
        };
        if deleting && !footprint.locked {
            changes.push(PlannedChange::Delete {
                kiid: footprint.kiid.clone(),
                reference: footprint.reference.clone(),
                footprint_id: footprint.footprint_id.clone(),
                reason,
            });
            counts.deleted.planned += 1;
            continue;
        }
        if deleting {
            notes.push(conflict(
                "footprint_locked_not_deleted",
                format!(
                    "{} {}, but it is locked, so it is kept; unlock it and sync again to \
                     delete it",
                    footprint.reference,
                    reason.describe()
                ),
                Some(&footprint.reference),
            ));
        }
        without_symbols.push(FootprintWithoutSymbol {
            reference: footprint.reference.clone(),
            kiid: footprint.kiid.clone(),
            footprint_id: footprint.footprint_id.clone(),
            locked: footprint.locked,
            reason,
        });
    }
    counts.board_only_preserved.planned = unmatched - counts.deleted.planned;
    let status = if !diagnostics.is_empty() {
        PlanStatus::Conflict
    } else if changes.is_empty() {
        PlanStatus::Noop
    } else {
        PlanStatus::Ready
    };
    diagnostics.extend(notes);
    let plan_revision = plan_revision(netlist_source, board);
    SyncPlan {
        status,
        plan_revision,
        counts,
        changes,
        diagnostics,
        unassigned,
        // Set by the handler's preflight, which reads the library.
        skipped: Vec::new(),
        refused_updates,
        without_symbols,
        // Set by the handler, from the vias and zones the snapshot read.
        copper_net_changes: Vec::new(),
        // Set by the handler from the snapshot this plan was made against.
        staging: None,
        acknowledged_apply: None,
    }
}

fn conflict(code: &str, message: String, reference: Option<&str>) -> SyncDiagnostic {
    SyncDiagnostic {
        code: code.to_string(),
        message,
        reference: reference.map(str::to_string),
        references: reference.map(str::to_string).into_iter().collect(),
        footprint_id: None,
        net_change: None,
        kiid: None,
    }
}

/// A diagnostic about one library footprint and every part that uses it.
/// `reference` keeps its single-part meaning, so it is set only when exactly
/// one part is concerned.
fn footprint_conflict(
    code: &str,
    message: String,
    footprint_id: &str,
    references: Vec<String>,
) -> SyncDiagnostic {
    SyncDiagnostic {
        code: code.to_string(),
        message,
        reference: match references.as_slice() {
            [only] => Some(only.clone()),
            _ => None,
        },
        references,
        footprint_id: Some(footprint_id.to_string()),
        net_change: None,
        kiid: None,
    }
}

/// References for a message, bounded: a common footprint can block hundreds
/// of parts, and the complete list is in the diagnostic's `references`.
fn listed(references: &[String]) -> String {
    const SHOWN: usize = 8;
    let shown = references
        .iter()
        .take(SHOWN)
        .map(String::as_str)
        .collect::<Vec<_>>()
        .join(", ");
    match references.len().saturating_sub(SHOWN) {
        0 => shown,
        more => format!("{shown} and {more} more"),
    }
}

/// The schematic connects a pad the footprint does not have. `missing` is
/// never empty.
fn pad_missing_conflict(reference: &str, footprint_id: &str, missing: &[&str]) -> SyncDiagnostic {
    let pads = if missing.len() == 1 { "pad" } else { "pads" };
    footprint_conflict(
        "footprint_pad_missing",
        format!(
            "the schematic connects {reference} {pads} {}, which footprint {footprint_id} does not have",
            missing.join(", ")
        ),
        footprint_id,
        vec![reference.to_string()],
    )
}

/// Pad numbers the schematic connects that `available` does not contain.
fn missing_pads<'a>(
    pad_nets: &'a BTreeMap<String, String>,
    available: &BTreeSet<String>,
) -> Vec<&'a str> {
    pad_nets
        .keys()
        .filter(|number| !available.contains(*number))
        .map(String::as_str)
        .collect()
}

/// The pad nets a footprint with the pads `available` takes: every pin's but
/// those of the pins KiCad left unconnected that it has no pad for. Such a
/// pin connects nothing, so the footprint of a keyed header, which has no pad
/// at the key, is synced, and its pad nets name only pads it has. KiCad's
/// Update PCB likewise sets the nets of the pads a footprint has
/// (`updateComponentPadConnections`, KiCad 10.0.8).
fn nets_for_pads(
    pad_nets: &BTreeMap<String, String>,
    unconnected_pins: &BTreeSet<String>,
    available: &BTreeSet<String>,
) -> BTreeMap<String, String> {
    pad_nets
        .iter()
        .filter(|(number, _)| available.contains(*number) || !unconnected_pins.contains(*number))
        .map(|(number, net)| (number.clone(), net.clone()))
        .collect()
}

/// A stable identity for the design-bearing netlist sections.
///
/// `kicad-cli sch export netlist` stamps `(date "…T14:48:16")` and the
/// exporting tool's version into every export, so hashing the raw source
/// yields a different revision **every second** for a design nobody touched —
/// and since apply requires the revision a dry run returned, apply could only
/// ever succeed if both calls landed inside the same wall-clock second. That
/// is a race, not a guarantee: it passes on a fast machine and fails on a
/// human reviewing the plan first, which is the whole point of the plan.
///
/// The revision must cover what the plan *read*: the complete top-level
/// `components` and `nets` trees. Hashing those trees structurally ignores the
/// volatile header without confusing nested nodes or quoted text for header
/// metadata.
fn netlist_identity(netlist_source: &str) -> Vec<u8> {
    let Ok(root) = copperpour_sexp::parse_sexp(netlist_source) else {
        // Production reaches this function only after successful netlist
        // parsing. Keeping invalid synthetic planner inputs distinct makes the
        // pure planner tests useful without creating a second error path here.
        return netlist_source.as_bytes().to_vec();
    };

    let mut identity = Vec::new();
    for tag in ["components", "nets"] {
        match root.find(tag) {
            Some(node) => {
                identity.push(1);
                append_sexp_identity(node, &mut identity);
            }
            None => identity.push(0),
        }
    }
    identity
}

fn append_sexp_identity(node: &SexpNode, identity: &mut Vec<u8>) {
    match node {
        SexpNode::Atom(value) => {
            identity.push(0);
            append_identity_bytes(value.as_bytes(), identity);
        }
        SexpNode::Str(value) => {
            identity.push(1);
            append_identity_bytes(value.as_bytes(), identity);
        }
        SexpNode::List(children) => {
            identity.push(2);
            identity.extend_from_slice(&(children.len() as u64).to_le_bytes());
            for child in children {
                append_sexp_identity(child, identity);
            }
        }
    }
}

fn append_identity_bytes(value: &[u8], identity: &mut Vec<u8>) {
    identity.extend_from_slice(&(value.len() as u64).to_le_bytes());
    identity.extend_from_slice(value);
}

fn plan_revision(netlist_source: &str, board: &BoardState) -> String {
    let mut footprints = board.footprints.iter().collect::<Vec<_>>();
    footprints.sort_by(|a, b| a.kiid.cmp(&b.kiid));
    let mut hasher = Sha256::new();
    hasher.update(netlist_identity(netlist_source));
    hasher.update(serde_json::to_vec(&board.bounds).expect("bounds serialize"));
    for footprint in footprints {
        hasher.update(footprint.kiid.as_bytes());
        hasher.update(footprint.reference.as_bytes());
        hasher.update(footprint.footprint_id.as_bytes());
        hasher.update(footprint.symbol_path.as_deref().unwrap_or("").as_bytes());
        for (pad, net) in &footprint.pad_nets {
            hasher.update(pad.as_bytes());
            hasher.update(net.as_bytes());
        }
    }
    for (net, count) in &board.routed_nets {
        hasher.update(net.as_bytes());
        hasher.update(count.to_le_bytes());
    }
    encode_hex(&hasher.finalize())
}

fn refresh_revision_with_staging(plan: &mut SyncPlan) {
    let mut hasher = Sha256::new();
    hasher.update(plan.plan_revision.as_bytes());
    hasher.update(serde_json::to_vec(&plan.changes).expect("planned changes serialize"));
    // What was left out is part of what was reviewed.
    hasher.update(serde_json::to_vec(&plan.skipped).expect("skipped parts serialize"));
    hasher.update(
        serde_json::to_vec(&plan.copper_net_changes).expect("copper net changes serialize"),
    );
    plan.plan_revision = encode_hex(&hasher.finalize());
}

fn parse_exported_netlist(source: &str) -> Result<ExportedDesign> {
    let root = copperpour_sexp::parse_sexp(source).context("invalid KiCad netlist S-expression")?;
    let components_node = root
        .find("components")
        .context("KiCad netlist has no components section")?;

    let mut components = Vec::new();
    let mut by_reference = HashMap::new();
    let mut unassigned = Vec::new();
    let mut unassigned_references = HashSet::new();
    let mut seen_references = HashSet::new();
    for component_node in components_node.find_all("comp") {
        let reference = required_value(component_node, "ref")?;
        // One invariant for every exported component, checked before the
        // footprint branch: an unassigned component never enters
        // `by_reference`, so a check there alone let a reference repeat
        // across the two roles and reach the plan (#507 review).
        if !seen_references.insert(reference.clone()) {
            bail!("KiCad netlist contains duplicate component reference {reference}");
        }

        let sheet_stamp = component_node
            .find("sheetpath")
            .and_then(|sheet| sheet.find_str("tstamps"))
            .context("KiCad netlist component has no sheet timestamp")?;
        // One UUID per unit of a multi-unit symbol, every one of which KiCad
        // matches a footprint by (`KICAD_NETLIST_PARSER::parseComponent`).
        let symbol_stamps = component_node
            .find("tstamps")
            .and_then(SexpNode::children)
            .map(|children| {
                children
                    .iter()
                    .skip(1)
                    .filter_map(SexpNode::as_str)
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
        let (symbol_stamp, other_stamps) = symbol_stamps
            .split_first()
            .context("KiCad netlist component has no symbol timestamp")?;
        let unit_path = |stamp: &str| {
            format!(
                "/{}/{}",
                sheet_stamp.trim_matches('/'),
                stamp.trim_matches('/')
            )
            .replace("//", "/")
        };
        let symbol_path = unit_path(symbol_stamp);
        let other_unit_paths = other_stamps
            .iter()
            .map(|stamp| unit_path(stamp))
            .collect::<Vec<_>>();
        let dnp = component_node.find_all("property").iter().any(|property| {
            property.find_str("name") == Some("dnp")
                || property.get(1).and_then(SexpNode::as_str) == Some("dnp")
        });
        let exclude_from_bom = component_node.find_all("property").iter().any(|property| {
            property.find_str("name") == Some("exclude_from_bom")
                || property.get(1).and_then(SexpNode::as_str) == Some("exclude_from_bom")
        });

        let value = required_value(component_node, "value")?;
        // kicad-cli writes no `(footprint …)` node at all for a symbol whose
        // Footprint property is empty — only a bare `(field (name
        // "Footprint"))`. That is a legitimate state (a generic `Device:R`
        // whose package has not been chosen yet), and it used to fail the
        // whole sync for every other component with it (#507).
        let footprint_id = component_node
            .find_str("footprint")
            .map(str::trim)
            .filter(|footprint| !footprint.is_empty())
            .map(str::to_owned);
        let Some(footprint_id) = footprint_id else {
            let lib_id = component_node.find("libsource").and_then(|source| {
                let lib = source.find_str("lib")?;
                let part = source.find_str("part")?;
                (!lib.is_empty() || !part.is_empty()).then(|| format!("{lib}:{part}"))
            });
            unassigned_references.insert(reference.clone());
            unassigned.push(UnassignedComponent {
                reference,
                value,
                lib_id,
                symbol_path,
            });
            continue;
        };

        let index = components.len();
        by_reference.insert(reference.clone(), index);
        components.push(DesignComponent {
            reference,
            value,
            footprint_id,
            symbol_path,
            other_unit_paths,
            dnp,
            exclude_from_bom,
            pad_nets: BTreeMap::new(),
            unconnected_pins: BTreeSet::new(),
            fields: symbol_fields(component_node),
            sheet: symbol_sheet(component_node),
        });
    }

    if components.is_empty() && unassigned.is_empty() {
        bail!("KiCad netlist contains zero components");
    }

    let mut net_names = BTreeSet::new();
    if let Some(nets_node) = root.find("nets") {
        for net_node in nets_node.find_all("net") {
            let net_name = required_value(net_node, "name")?;
            let nodes = net_node.find_all("node");
            // KiCad names the net of a pin wired to nothing
            // `unconnected-(<ref>-<pin>-Pad<n>)`, and a pin with a no-connect
            // flag, or of the no-connect type, has `no_connect` in its pin
            // type. Alone on its net, such a pin connects nothing. A pin with
            // a no-connect flag that is wired to other pins is still
            // connected, and so is a named net of one pin.
            if let [node] = nodes.as_slice() {
                let no_connect = node
                    .find_str("pintype")
                    .is_some_and(|pin_type| pin_type.contains("no_connect"));
                if no_connect || net_name.starts_with("unconnected-") {
                    let reference = required_value(node, "ref")?;
                    let pin = required_value(node, "pin")?;
                    if let Some(&index) = by_reference.get(&reference) {
                        components[index].unconnected_pins.insert(pin);
                    }
                }
            }
            for node in nodes {
                let reference = required_value(node, "ref")?;
                let pin = required_value(node, "pin")?;
                // KiCad's netlist holds the net for any component's pin,
                // whether or not the component has a footprint.
                if !net_name.is_empty() {
                    net_names.insert(net_name.clone());
                }
                let Some(&index) = by_reference.get(&reference) else {
                    // A wired pin of a component with no footprint: there is
                    // no pad to carry the net, so the node is dropped, not
                    // fatal.
                    if unassigned_references.contains(&reference) {
                        continue;
                    }
                    bail!("net {net_name} refers to unknown component {reference}");
                };
                if components[index]
                    .pad_nets
                    .insert(pin.clone(), net_name.clone())
                    .is_some()
                {
                    bail!("component {reference} pad {pin} appears in more than one net");
                }
            }
        }
    }

    Ok(ExportedDesign {
        root_sheet_uuid: None,
        components,
        skipped: Vec::new(),
        unassigned,
        net_names,
    })
}

/// The fields KiCad's Update PCB copies from a netlist `(comp …)`: every
/// `(fields (field (name …) "value"))` but Reference, Value, Footprint and
/// Component Class, an empty one included. A repeated name keeps its first
/// position and its last value, as KiCad's reader does (#788).
fn symbol_fields(component_node: &SexpNode) -> Vec<SymbolField> {
    let mut fields = Vec::new();
    let Some(listed) = component_node.find("fields") else {
        return fields;
    };
    for field in listed.find_all("field") {
        let Some(name) = field.find_str("name").filter(|name| !name.is_empty()) else {
            continue;
        };
        if matches!(
            name,
            "Reference" | "Value" | "Footprint" | "Component Class"
        ) {
            continue;
        }
        let value = field
            .children()
            .unwrap_or_default()
            .iter()
            .skip(1)
            .find_map(SexpNode::as_str)
            .unwrap_or_default()
            .to_string();
        match fields.iter_mut().find(|known| known.name == name) {
            Some(known) => known.value = value,
            None => fields.push(SymbolField {
                name: name.to_string(),
                value,
            }),
        }
    }
    fields
}

/// The sheet name, sheet file and footprint filters KiCad's Update PCB gives a
/// `(comp …)`'s footprint: the sheet path's `names`, or the `Sheetname`
/// property when they are empty, the `Sheetfile` property and the
/// `ki_fp_filters` property. A property named twice keeps its last value, as
/// KiCad's reader does.
fn symbol_sheet(component_node: &SexpNode) -> SymbolSheet {
    let property = |name: &str| {
        component_node
            .find_all("property")
            .into_iter()
            .rev()
            .find(|property| property.find_str("name") == Some(name))
            .and_then(|property| property.find_str("value"))
            .unwrap_or_default()
            .to_string()
    };
    let names = component_node
        .find("sheetpath")
        .and_then(|sheet| sheet.find_str("names"))
        .filter(|names| !names.is_empty());
    SymbolSheet {
        sheet_name: names.map_or_else(|| property("Sheetname"), str::to_owned),
        sheet_file: property("Sheetfile"),
        footprint_filters: property("ki_fp_filters"),
    }
}

fn required_value(node: &SexpNode, tag: &str) -> Result<String> {
    node.find_str(tag)
        .map(str::to_owned)
        .with_context(|| format!("KiCad netlist node is missing {tag}"))
}

fn update_footprint_item(
    item: &prost_types::Any,
    change: &PlannedChange,
    net_codes: &BTreeMap<String, i32>,
) -> Result<prost_types::Any> {
    use copperpour_ipc::gen::kiapi;
    use prost::Message;

    let PlannedChange::Update {
        kiid,
        reference,
        value,
        symbol_path,
        dnp,
        exclude_from_bom,
        pad_nets,
        fields,
        sheet,
        ..
    } = change
    else {
        bail!("an add change cannot update an existing footprint");
    };
    let mut footprint = kiapi::board::types::FootprintInstance::decode(item.value.as_slice())
        .context("KiCad returned an invalid footprint item")?;
    if footprint.id.as_ref().map(|id| id.value.as_str()) != Some(kiid.as_str()) {
        bail!("planned footprint {kiid} no longer matches the live board item");
    }

    apply_symbol_fields(&mut footprint, fields)?;
    apply_symbol_sheet(&mut footprint, sheet);
    apply_footprint_fields(
        &mut footprint,
        reference,
        value,
        symbol_path,
        *dnp,
        *exclude_from_bom,
        pad_nets,
        net_codes,
    )?;

    Ok(copperpour_ipc::builders::pack_any(
        &footprint,
        "kiapi.board.types.FootprintInstance",
    ))
}

#[allow(clippy::too_many_arguments)]
fn apply_footprint_fields(
    footprint: &mut copperpour_ipc::gen::kiapi::board::types::FootprintInstance,
    reference: &str,
    value: &str,
    symbol_path: &str,
    dnp: bool,
    exclude_from_bom: bool,
    pad_nets: &BTreeMap<String, String>,
    net_codes: &BTreeMap<String, i32>,
) -> Result<()> {
    use copperpour_ipc::gen::kiapi;

    set_field_text(&mut footprint.reference_field, "Reference", reference);
    set_field_text(&mut footprint.value_field, "Value", value);
    let definition = footprint
        .definition
        .as_mut()
        .context("board footprint has no library definition")?;
    set_field_text(&mut definition.reference_field, "Reference", reference);
    set_field_text(&mut definition.value_field, "Value", value);

    footprint.symbol_path = Some(kiapi::common::types::SheetPath {
        path: symbol_path
            .split('/')
            .filter(|part| !part.is_empty())
            .map(|part| kiapi::common::types::Kiid {
                value: part.to_string(),
            })
            .collect(),
        path_human_readable: String::new(),
    });
    for attributes in [&mut footprint.attributes, &mut definition.attributes] {
        let attributes = attributes.get_or_insert_with(Default::default);
        attributes.do_not_populate = dnp;
        attributes.exclude_from_bill_of_materials = exclude_from_bom;
    }

    let mut seen_pads = std::collections::HashSet::new();
    for child in &mut definition.items {
        // `definition.items` mixes pads, graphics and text in one repeated
        // field, so the type URL is the only sound discriminator. Filtering by
        // "did `Pad::decode` succeed" instead accepted every graphic — proto3
        // skips unrecognised field numbers rather than failing — and the write
        // back below then re-typed each one as a pad, so every footprint this
        // tool touched lost its artwork and gained a nameless pad at (0,0)
        // for each shape it used to have (#244).
        if !copperpour_ipc::builders::any_is(child, "kiapi.board.types.Pad") {
            continue;
        }
        // A child that *declares* itself a pad and will not decode is a real
        // failure, not something to skip past silently.
        let mut pad =
            kiapi::board::types::Pad::decode(child.value.as_slice()).with_context(|| {
                format!(
                    "footprint {reference} has a pad KiCad sent in a form Copperpour cannot read"
                )
            })?;
        seen_pads.insert(pad.number.clone());
        pad.net = pad_nets
            .get(&pad.number)
            .map(|name| kiapi::board::types::Net {
                // Net codes are KiCad-internal. Preserve a resolved live code
                // when one exists; for a schematic-only net, the name is the
                // public identity and lets KiCad create the new board net.
                code: net_codes
                    .get(name)
                    .copied()
                    .map(|value| kiapi::board::types::NetCode { value }),
                name: name.clone(),
            });
        *child = copperpour_ipc::builders::pack_any(&pad, "kiapi.board.types.Pad");
    }
    for number in pad_nets.keys() {
        if !seen_pads.contains(number) {
            bail!("footprint {reference} has no pad {number}");
        }
    }

    Ok(())
}

/// How many pads and how many drawn items a footprint carries, its instance
/// attributes, and the 3D model files it names.
///
/// The two counts are the numbers #244 got wrong in opposite directions: every
/// graphic became a pad, so pads went up by exactly the number of drawings, and
/// drawings went to zero. The attributes and models are what #789's sync used
/// to leave off.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
struct FootprintShape {
    pads: usize,
    drawings: usize,
    attributes: AttributeSet,
    /// Sorted, so the comparison does not depend on the order KiCad lists them.
    model_files: Vec<String>,
    /// Every field by name: the mandatory four and the custom ones (#789).
    fields: BTreeMap<String, FieldShape>,
    /// Each pad with a fabrication property, by number, sorted (#149). A pad
    /// KiCad reports as `PFP_NONE`, or before 10.0.7 not at all, has none.
    pad_properties: Vec<(String, PadFabricationProperty)>,
    /// Each pad's zone connection style, by number, sorted.
    pad_zones: Vec<(String, i32)>,
    /// Number of reference points on the footprint.
    reference_points: usize,
    /// Its sheet name, sheet file and footprint filters (#237).
    sheet: SymbolSheet,
}

/// What a field shows: its text, its layer, whether it is visible and
/// whether KiCad keeps it upright as the footprint turns.
#[derive(Debug, Clone, PartialEq, Eq)]
struct FieldShape {
    text: String,
    layer: i32,
    visible: bool,
    upright: bool,
}

impl FieldShape {
    fn of(field: &copperpour_ipc::gen::kiapi::board::types::Field) -> Self {
        let board_text = field.text.as_ref();
        Self {
            text: board_text
                .and_then(|text| text.text.as_ref())
                .map(|text| text.text.clone())
                .unwrap_or_default(),
            layer: board_text.map(|text| text.layer).unwrap_or_default(),
            visible: field.visible,
            upright: board_text
                .and_then(|text| text.text.as_ref())
                .and_then(|text| text.attributes.as_ref())
                .is_some_and(|attributes| attributes.keep_upright),
        }
    }

    fn listed(&self) -> String {
        let layer = copperpour_ipc::gen::kiapi::board::types::BoardLayer::try_from(self.layer)
            .ok()
            .and_then(copperpour_ipc::builders::layer_name)
            .unwrap_or("unknown layer");
        let shown = if self.visible { "visible" } else { "hidden" };
        let upright = if self.upright {
            "kept upright"
        } else {
            "not kept upright"
        };
        format!("{:?} on {layer}, {shown}, {upright}", self.text)
    }
}

/// Every instance attribute KiCad reports for a footprint, named by the
/// `(attr …)` token a footprint file uses for it. The set includes DNP as it
/// was finally sent, after the schematic overrode the library.
///
/// A message with no attributes, or with every flag clear and no mounting
/// style, is the same empty set: KiCad reports both as an unspecified part.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
struct AttributeSet(BTreeSet<&'static str>);

impl AttributeSet {
    fn of(
        attributes: Option<&copperpour_ipc::gen::kiapi::board::types::FootprintAttributes>,
    ) -> Self {
        use copperpour_ipc::gen::kiapi::board::types::FootprintMountingStyle;
        let mut names = BTreeSet::new();
        if let Some(attributes) = attributes {
            match attributes.mounting_style() {
                FootprintMountingStyle::FmsSmd => {
                    names.insert("smd");
                }
                FootprintMountingStyle::FmsThroughHole => {
                    names.insert("through_hole");
                }
                _ => {}
            }
            for (set, name) in [
                (attributes.not_in_schematic, "board_only"),
                (
                    attributes.exclude_from_position_files,
                    "exclude_from_pos_files",
                ),
                (
                    attributes.exclude_from_bill_of_materials,
                    "exclude_from_bom",
                ),
                (
                    attributes.exempt_from_courtyard_requirement,
                    "allow_missing_courtyard",
                ),
                (attributes.do_not_populate, "dnp"),
                (
                    attributes.allow_soldermask_bridges,
                    "allow_soldermask_bridges",
                ),
            ] {
                if set {
                    names.insert(name);
                }
            }
        }
        Self(names)
    }

    fn listed(&self) -> String {
        self.0.iter().copied().collect::<Vec<_>>().join(", ")
    }
}

/// Tally the pads and drawings of each footprint in a set of packed items,
/// keyed by reference.
fn footprint_shapes<'a>(
    items: impl Iterator<Item = &'a prost_types::Any>,
) -> BTreeMap<String, FootprintShape> {
    use copperpour_ipc::gen::kiapi;
    use prost::Message;

    let mut out = BTreeMap::new();
    for item in items {
        let Ok(footprint) = kiapi::board::types::FootprintInstance::decode(item.value.as_slice())
        else {
            continue;
        };
        let Some(definition) = footprint.definition.as_ref() else {
            continue;
        };
        let reference = field_text(&footprint.reference_field);
        if reference.is_empty() {
            continue;
        }
        let mut shape = FootprintShape {
            attributes: AttributeSet::of(footprint.attributes.as_ref()),
            sheet: SymbolSheet::of(&footprint),
            ..Default::default()
        };
        for (name, field) in [
            ("Reference", &footprint.reference_field),
            ("Value", &footprint.value_field),
            ("Datasheet", &footprint.datasheet_field),
            ("Description", &footprint.description_field),
        ] {
            if let Some(field) = field {
                shape.fields.insert(name.to_string(), FieldShape::of(field));
            }
        }
        for child in &definition.items {
            match copperpour_ipc::builders::any_type_name(child) {
                "kiapi.board.types.Pad" => {
                    shape.pads += 1;
                    if let Ok(pad) = kiapi::board::types::Pad::decode(child.value.as_slice()) {
                        if let Some(property) = reported_fab_property(pad.fab_property) {
                            shape.pad_properties.push((pad.number.clone(), property));
                        }
                        if let Some(zone) = pad
                            .pad_stack
                            .as_ref()
                            .and_then(|stack| stack.zone_settings.as_ref())
                        {
                            shape.pad_zones.push((pad.number, zone.zone_connection));
                        }
                    }
                }
                "kiapi.board.types.ReferencePoint" => shape.reference_points += 1,
                "kiapi.board.types.BoardGraphicShape" | "kiapi.board.types.BoardText" => {
                    shape.drawings += 1
                }
                "kiapi.board.types.Footprint3DModel" => {
                    if let Ok(model) =
                        kiapi::board::types::Footprint3DModel::decode(child.value.as_slice())
                    {
                        shape.model_files.push(model.filename);
                    }
                }
                "kiapi.board.types.Field" => {
                    if let Ok(field) = kiapi::board::types::Field::decode(child.value.as_slice()) {
                        shape
                            .fields
                            .insert(field.name.clone(), FieldShape::of(&field));
                    }
                }
                _ => {}
            }
        }
        shape.model_files.sort();
        shape.pad_properties.sort();
        shape.pad_zones.sort();
        out.insert(reference, shape);
    }
    out
}

/// Read the board back and hold it to what was just sent.
///
/// `create_items`/`update_items` only confirm that KiCad *accepted* each item,
/// and the counts this tool reports are copied from the plan — so when #244
/// turned every footprint graphic into a nameless pad, KiCad returned ISC_OK
/// for each one and the response said the sync succeeded. Nothing anywhere
/// looked at what actually landed.
///
/// This is a backstop for that class, not for that bug: with the type-URL fix
/// in place it should never fire. `delete_footprint` already re-queries after
/// mutating; this follows it.
///
/// **It fails the call only on a gained pad.** KiCad has no business inventing
/// one, so that is unambiguous and is #244's exact signature. A *drop* in
/// drawings is reported instead of refused, because it has a benign
/// explanation this check cannot yet rule out — KiCad re-creates a footprint's
/// children from the message on deserialize, and if it promotes a `BoardText`
/// child into a `Field` (which this tally deliberately ignores) the count
/// would fall without anything being wrong. Turning a working sync into an
/// error over that is worse than the warning. Tighten it once it has been
/// watched against a live KiCad; see the note on #244.
fn verify_board_matches_what_was_sent(
    client: &copperpour_ipc::KiCadIpcClient,
    document: &copperpour_ipc::gen::kiapi::common::types::DocumentSpecifier,
    expected: &BTreeMap<String, FootprintShape>,
) -> Result<Vec<String>> {
    use copperpour_ipc::gen::kiapi;

    if expected.is_empty() {
        return Ok(Vec::new());
    }
    let items = client.get_items_in(
        document.clone(),
        kiapi::common::types::KiCadObjectType::KotPcbFootprint,
    )?;
    let actual = footprint_shapes(items.iter());

    let mut corrupted = Vec::new();
    let mut suspicious = Vec::new();
    for (reference, want) in expected {
        // A reference the read-back cannot see is its own problem, but not this
        // check's: KiCad may name it differently after a rename, and failing
        // here would turn a successful sync into an error over bookkeeping.
        let Some(got) = actual.get(reference) else {
            continue;
        };
        let detail = format!(
            "{reference}: sent {} pads and {} drawings, board now has {} and {}",
            want.pads, want.drawings, got.pads, got.drawings
        );
        if got.pads > want.pads {
            corrupted.push(detail);
        } else if (got.pads, got.drawings) != (want.pads, want.drawings) {
            suspicious.push(detail);
        }
        if got.attributes != want.attributes {
            suspicious.push(format!(
                "{reference}: sent attributes [{}], board now has [{}]",
                want.attributes.listed(),
                got.attributes.listed()
            ));
        }
        if got.pad_properties != want.pad_properties {
            // Only the pads that differ: on a BGA both whole lists would be
            // cut to the same first entries.
            let lost = sorted_difference(&want.pad_properties, &got.pad_properties);
            let gained = sorted_difference(&got.pad_properties, &want.pad_properties);
            suspicious.push(format!(
                "{reference}: pad fabrication properties sent but not on the board [{}], \
                 on the board but not sent [{}]",
                listed_pad_properties(lost),
                listed_pad_properties(gained)
            ));
        }
        if got.pad_zones != want.pad_zones {
            suspicious.push(format!(
                "{reference}: sent {} pad zone settings, board now has {}",
                want.pad_zones.len(),
                got.pad_zones.len()
            ));
        }
        if got.reference_points != want.reference_points {
            suspicious.push(format!(
                "{reference}: sent {} reference point(s), board now has {}",
                want.reference_points, got.reference_points
            ));
        }
        if got.model_files != want.model_files {
            suspicious.push(format!(
                "{reference}: sent 3D models [{}], board now has [{}]",
                want.model_files.join(", "),
                got.model_files.join(", ")
            ));
        }
        if got.sheet != want.sheet {
            suspicious.push(format!(
                "{reference}: sent {}, board now has {}",
                want.sheet.listed(),
                got.sheet.listed()
            ));
        }
        // Only the fields that were sent: KiCad fills in a Datasheet and a
        // Description of its own when none was sent, and those are not a loss.
        for (name, sent) in &want.fields {
            match got.fields.get(name) {
                Some(kept) if kept == sent => {}
                Some(kept) => suspicious.push(format!(
                    "{reference}: sent field {name} as {}, board now has {}",
                    sent.listed(),
                    kept.listed()
                )),
                None => suspicious.push(format!(
                    "{reference}: sent field {name} as {}, board now has none",
                    sent.listed()
                )),
            }
        }
    }
    if !corrupted.is_empty() {
        bail!(
            "KiCad's board gained pads this sync never sent, so the footprints on \
             it are not the ones that were planned — inspect the board and do not \
             save it: {}",
            corrupted.join("; ")
        );
    }
    Ok(suspicious)
}

/// What an apply that took acknowledged net changes leaves for DRC (#38): the
/// routed copper on each net they name, counted before the commit and read
/// back after it, and kicad-cli's DRC of the board KiCad now holds under the
/// project's rules, checked as `apply_specctra_ses` checks its candidate. The
/// board file is not saved.
fn acknowledged_apply_evidence(
    client: &copperpour_ipc::KiCadIpcClient,
    snapshot: &LiveSnapshot,
    plan: &SyncPlan,
    runtime: &tokio::runtime::Handle,
    cli: &str,
    board: &Path,
) -> Result<AcknowledgedApply> {
    let nets = plan
        .diagnostics
        .iter()
        .filter(|diagnostic| diagnostic.code == "routed_pad_net_change_acknowledged")
        .filter_map(|diagnostic| diagnostic.net_change.as_ref())
        .flat_map(|change| [change.from.as_str(), change.to.as_str()])
        .filter(|net| !net.is_empty())
        .collect::<BTreeSet<_>>();
    let (after, unreadable_zone) = read_routed_copper(client, &snapshot.document)?;
    if unreadable_zone {
        bail!(
            "KiCad read back a copper zone whose net cannot be read, so the copper on the \
             named nets is not known"
        );
    }
    let routed_copper = nets
        .into_iter()
        .map(|net| RoutedCopper {
            net: net.to_string(),
            before_count: snapshot.routed_copper.get(net).copied().unwrap_or(0),
            after_count: after.get(net).copied().unwrap_or(0),
        })
        .collect();

    let text = client.save_document_to_string_in(snapshot.document.clone())?;
    let dir = tempfile::tempdir()?;
    let candidate = dir.path().join("synced.kicad_pcb");
    std::fs::write(&candidate, text).with_context(|| format!("write {}", candidate.display()))?;
    let (report, project_rules_applied) = runtime.block_on(super::cli::run_drc_as_project(
        cli, &candidate, board, false,
    ))?;
    let violations = report.violations.len();
    let unconnected_items = report.unconnected_items.as_ref().map_or(0, Vec::len);
    let schematic_parity_violations = report.schematic_parity.as_ref().map_or(0, Vec::len);
    Ok(AcknowledgedApply {
        routed_copper,
        drc: serde_json::json!({
            "clean": violations == 0
                && unconnected_items == 0
                && schematic_parity_violations == 0,
            "violations": violations,
            "unconnected_items": unconnected_items,
            "schematic_parity_violations": schematic_parity_violations,
            "project_rules_applied": project_rules_applied,
            "creepage_skipped": report.creepage_skipped
        }),
    })
}

fn set_field_text(
    field: &mut Option<copperpour_ipc::gen::kiapi::board::types::Field>,
    name: &str,
    value: &str,
) {
    let field = field.get_or_insert_with(Default::default);
    field.name = name.to_string();
    let board_text = field.text.get_or_insert_with(Default::default);
    board_text.text.get_or_insert_with(Default::default).text = value.to_string();
}

fn saved_hierarchy_files(root: &Path) -> Result<Vec<PathBuf>> {
    fn visit(
        path: &Path,
        seen: &mut HashSet<PathBuf>,
        active: &mut HashSet<PathBuf>,
        files: &mut Vec<PathBuf>,
    ) -> Result<()> {
        let canonical = path
            .canonicalize()
            .with_context(|| format!("cannot resolve schematic {}", path.display()))?;
        if active.contains(&canonical) {
            bail!(
                "schematic hierarchy contains a cycle at {}",
                display_path(&canonical)
            );
        }
        if !seen.insert(canonical.clone()) {
            return Ok(());
        }
        active.insert(canonical.clone());
        let name = canonical
            .file_name()
            .and_then(|name| name.to_str())
            .context("schematic path has no file name")?;
        let lock = canonical.with_file_name(format!("~{name}.lck"));
        if lock.exists() {
            bail!(
                "{} is open in the schematic editor; save and close the hierarchy before syncing",
                display_path(&canonical)
            );
        }
        let schematic = copperpour_schematic_editor::Schematic::load(&canonical)
            .with_context(|| format!("cannot load schematic {}", display_path(&canonical)))?;
        files.push(canonical.clone());
        let parent = canonical.parent().unwrap_or_else(|| Path::new("."));
        for sheet in schematic.sheets.iter() {
            let child = parent.join(sheet.file());
            if !child.exists() {
                bail!(
                    "hierarchical sheet {} referenced by {} does not exist",
                    display_path(&child),
                    display_path(&canonical)
                );
            }
            visit(&child, seen, active, files)?;
        }
        active.remove(&canonical);
        Ok(())
    }

    let mut files = Vec::new();
    visit(root, &mut HashSet::new(), &mut HashSet::new(), &mut files)?;
    Ok(files)
}

fn apply_saved_symbol_flags(files: &[PathBuf], design: &mut ExportedDesign) -> Result<()> {
    #[derive(Debug)]
    struct Flags {
        reference: String,
        symbol_path: String,
        in_bom: bool,
        on_board: bool,
        dnp: bool,
    }

    let mut flags = Vec::new();
    for (index, file) in files.iter().enumerate() {
        let source = std::fs::read_to_string(file)?;
        let tree = copperpour_sexp::parse_sexp(&source)?;
        let file_uuid = tree.find_str("uuid").unwrap_or("");
        if index == 0 && !file_uuid.is_empty() {
            design.root_sheet_uuid = Some(file_uuid.to_string());
        }
        for symbol in tree.find_all("symbol") {
            let Some(uuid) = symbol.find_str("uuid") else {
                continue;
            };
            let in_bom = symbol.find_str("in_bom") != Some("no");
            let on_board = symbol.find_str("on_board") != Some("no");
            let dnp = symbol.find_str("dnp") == Some("yes");
            let projects = symbol
                .find("instances")
                .map(|instances| instances.find_all("project"))
                .unwrap_or_default();
            for project in projects {
                for path in project.find_all("path") {
                    let Some(reference) = path.find_str("reference") else {
                        continue;
                    };
                    let instance = path.get(1).and_then(SexpNode::as_str).unwrap_or("/");
                    let base = if instance == "/" && !file_uuid.is_empty() {
                        format!("/{file_uuid}")
                    } else {
                        instance.trim_end_matches('/').to_string()
                    };
                    flags.push(Flags {
                        reference: reference.to_string(),
                        symbol_path: format!("{base}/{uuid}").replace("//", "/"),
                        in_bom,
                        on_board,
                        dnp,
                    });
                }
            }
        }
    }

    for reference in flags
        .iter()
        .map(|entry| entry.reference.as_str())
        .collect::<HashSet<_>>()
    {
        let entries = flags
            .iter()
            .filter(|entry| entry.reference == reference)
            .collect::<Vec<_>>();
        if entries.iter().any(|entry| {
            entry.in_bom != entries[0].in_bom
                || entry.on_board != entries[0].on_board
                || entry.dnp != entries[0].dnp
        }) {
            bail!("multi-unit reference {reference} has inconsistent board/BOM/DNP flags");
        }
    }

    let root_uuid = design.root_sheet_uuid.as_deref();
    design.components.retain_mut(|component| {
        let component_path = normalize_symbol_path(&component.symbol_path, root_uuid);
        let path_match = flags
            .iter()
            .find(|entry| normalize_symbol_path(&entry.symbol_path, root_uuid) == component_path);
        let reference_matches = flags
            .iter()
            .filter(|entry| entry.reference == component.reference)
            .collect::<Vec<_>>();
        let entry = path_match.or_else(|| reference_matches.first().copied());
        let Some(entry) = entry else {
            return true;
        };
        if !entry.on_board {
            design.skipped.push(SkippedComponent {
                reference: entry.reference.clone(),
                symbol_path: entry.symbol_path.clone(),
            });
            return false;
        }
        component.dnp = entry.dnp;
        component.exclude_from_bom = !entry.in_bom;
        true
    });
    // The same flag governs a component with no footprint: excluded from the
    // board it is skipped by flag rather than reported as unassigned. Only
    // Exclude from board keeps a part off the PCB, as in KiCad (#875).
    design.unassigned.retain(|component| {
        let component_path = normalize_symbol_path(&component.symbol_path, root_uuid);
        let path_match = flags
            .iter()
            .find(|entry| normalize_symbol_path(&entry.symbol_path, root_uuid) == component_path);
        let entry = path_match.or_else(|| {
            flags
                .iter()
                .find(|entry| entry.reference == component.reference)
        });
        let Some(entry) = entry else {
            return true;
        };
        if !entry.on_board {
            design.skipped.push(SkippedComponent {
                reference: entry.reference.clone(),
                symbol_path: entry.symbol_path.clone(),
            });
            return false;
        }
        true
    });
    let mut skipped_references = HashSet::new();
    for entry in flags.iter().filter(|entry| !entry.on_board) {
        if !skipped_references.insert(entry.reference.as_str()) {
            continue;
        }
        let entry_path = normalize_symbol_path(&entry.symbol_path, root_uuid);
        if !design
            .skipped
            .iter()
            .any(|skipped| normalize_symbol_path(&skipped.symbol_path, root_uuid) == entry_path)
        {
            design.skipped.push(SkippedComponent {
                reference: entry.reference.clone(),
                symbol_path: entry.symbol_path.clone(),
            });
        }
    }
    Ok(())
}

fn snapshot_board(client: &copperpour_ipc::KiCadIpcClient, board: &Path) -> Result<LiveSnapshot> {
    use copperpour_ipc::gen::kiapi;
    use kiapi::common::types::KiCadObjectType as ObjectType;

    let document = client.find_open_board(board)?;
    let footprint_items = client.get_items_in(document.clone(), ObjectType::KotPcbFootprint)?;
    let mut footprints = Vec::new();
    let mut items = BTreeMap::new();
    let mut pad_owners = BTreeMap::new();
    for item in footprint_items {
        let instance = kiapi::board::types::FootprintInstance::decode(item.value.as_slice())
            .context("KiCad returned an invalid footprint item")?;
        let footprint = board_footprint_from_instance(&instance)?;
        let kiid = footprint.kiid.clone();
        for child in instance
            .definition
            .iter()
            .flat_map(|definition| &definition.items)
        {
            if !copperpour_ipc::builders::any_is(child, "kiapi.board.types.Pad") {
                continue;
            }
            if let Ok(pad) = kiapi::board::types::Pad::decode(child.value.as_slice()) {
                if let Some(id) = pad.id.filter(|id| !id.value.is_empty()) {
                    pad_owners.insert(id.value, (kiid.clone(), pad.number));
                }
            }
        }
        footprints.push(footprint);
        items.insert(kiid, item);
    }

    let nets = client.get_nets_in(document.clone())?;
    let net_codes = nets
        .iter()
        .map(|net| (net.name.clone(), net.netcode))
        .collect::<BTreeMap<_, _>>();
    let (routed_copper, unreadable_zone) = read_routed_copper(client, &document)?;
    let (copper, copper_items) = read_copper_items(client, &document)?;
    let mut routed_nets = routed_copper.clone();
    if unreadable_zone {
        // A zone whose net cannot be read could pour any net, so every pad-net
        // reassignment fails closed.
        for net in net_codes.keys() {
            *routed_nets.entry(net.clone()).or_insert(0) += 1;
        }
    }
    // Every item KiCad holds, measured as KiCad measures it (#688). A board
    // with nothing measured stages from the origin. The classes KiCad would
    // not list are kept, so the response can say the extent is partial, and
    // that a board with nothing measured is not known to be empty.
    let bounds = client.get_board_bounds_in(document.clone())?;
    let staging = StagingEvidence::of(&bounds);
    let extents = bounds.extents.unwrap_or(copperpour_ipc::IpcBoardExtents {
        min: copperpour_ipc::IpcVector2 { x: 0.0, y: 0.0 },
        max: copperpour_ipc::IpcVector2 { x: 0.0, y: 0.0 },
    });
    Ok(LiveSnapshot {
        state: BoardState {
            footprints,
            routed_nets,
            bounds: Bounds {
                min_x: extents.min.x,
                min_y: extents.min.y,
                max_x: extents.max.x,
                max_y: extents.max.y,
            },
        },
        staging,
        items,
        net_codes,
        routed_copper,
        copper,
        copper_items,
        pad_owners,
        document,
    })
}

/// The board's vias and copper zones, in KiCad's order, with each item as
/// KiCad listed it, by KIID (#862). A rule area is not copper, and a zone
/// whose net cannot be read is left out: `read_routed_copper` already marks
/// every net routed for it.
fn read_copper_items(
    client: &copperpour_ipc::KiCadIpcClient,
    document: &copperpour_ipc::gen::kiapi::common::types::DocumentSpecifier,
) -> Result<(Vec<CopperItem>, BTreeMap<String, prost_types::Any>)> {
    use copperpour_ipc::gen::kiapi;
    use kiapi::common::types::KiCadObjectType as ObjectType;

    let mut copper = Vec::new();
    let mut items = BTreeMap::new();
    for item in client.get_items_in(document.clone(), ObjectType::KotPcbVia)? {
        let Ok(via) = kiapi::board::types::Via::decode(item.value.as_slice()) else {
            continue;
        };
        let Some(kiid) = via.id.map(|id| id.value).filter(|id| !id.is_empty()) else {
            continue;
        };
        copper.push(CopperItem {
            kind: CopperKind::Via,
            kiid: kiid.clone(),
            net: via.net.map(|net| net.name).unwrap_or_default(),
            name: String::new(),
        });
        items.insert(kiid, item);
    }
    for item in client.get_items_in(document.clone(), ObjectType::KotPcbZone)? {
        let ZoneNet::Copper(net) = zone_net(item.value.as_slice()) else {
            continue;
        };
        let Ok(zone) = kiapi::board::types::Zone::decode(item.value.as_slice()) else {
            continue;
        };
        let Some(kiid) = zone.id.map(|id| id.value).filter(|id| !id.is_empty()) else {
            continue;
        };
        copper.push(CopperItem {
            kind: CopperKind::Zone,
            kiid: kiid.clone(),
            net: net.name,
            name: zone.name,
        });
        items.insert(kiid, item);
    }
    Ok((copper, items))
}

/// Move the vias and copper zones whose net leaves the schematic as KiCad's
/// Update PCB moves them, and note each one it leaves (#862).
///
/// KiCad 10.0.8, `BOARD_NETLIST_UPDATER` (`board_netlist_updater.cpp`):
/// `updateComponentPadConnections` records each pad's old net and new net in
/// `m_oldToNewNets` when both are non-empty, the last pad winning (`:1202`).
/// `updateCopperZoneNets` (`:1676`) then looks at every via and copper zone
/// whose net no schematic pin carries. A via takes the new net the map gives
/// its old one (`:1703`), else KiCad warns "Via connected to unknown net". A
/// zone first takes the new net of a pad connected to it before the update
/// whose net changes (`cacheCopperZoneConnections`, `:1664`), then the map
/// (`:1796`), else KiCad warns that it "has no pads connected". A zone's net
/// counts as in the schematic when it starts with a schematic net's name, as
/// KiCad's `isInNetlist` reads pseudo net names.
///
/// The map is built from the pad changes still planned, so a part the
/// preflight skipped moves nothing. `zone_pads` holds the pads connected to
/// each zone, as `(footprint kiid, pad number)`, in the order to try them.
/// An item on no net is left alone without a note: there is nothing to move.
/// A plan with a conflict moves nothing.
fn plan_copper_nets(
    plan: &mut SyncPlan,
    board: &BoardState,
    copper: &[CopperItem],
    net_names: &BTreeSet<String>,
    zone_pads: &BTreeMap<String, Vec<(String, String)>>,
) {
    if plan.status == PlanStatus::Conflict {
        return;
    }
    let mut old_to_new = BTreeMap::<&str, &str>::new();
    let mut new_nets = BTreeMap::<(&str, &str), &str>::new();
    for change in &plan.changes {
        let PlannedChange::Update { kiid, pad_nets, .. } = change else {
            continue;
        };
        let Some(footprint) = board
            .footprints
            .iter()
            .find(|footprint| &footprint.kiid == kiid)
        else {
            continue;
        };
        for number in &footprint.pad_numbers {
            let old = footprint.pad_nets.get(number).map_or("", String::as_str);
            let new = pad_nets.get(number).map_or("", String::as_str);
            new_nets.insert((kiid.as_str(), number.as_str()), new);
            // `old != new` as KiCad's `if( pad->GetNetname() != netName )`
            // (`:1173`); a schematic net in the map would move nothing anyway,
            // as copper on schematic nets is skipped.
            if !old.is_empty() && !new.is_empty() && old != new {
                old_to_new.insert(old, new);
            }
        }
    }
    let net_after = |footprint_kiid: &str, number: &str| -> String {
        if let Some(net) = new_nets.get(&(footprint_kiid, number)) {
            return net.to_string();
        }
        board
            .footprints
            .iter()
            .find(|footprint| footprint.kiid == footprint_kiid)
            .and_then(|footprint| footprint.pad_nets.get(number))
            .cloned()
            .unwrap_or_default()
    };
    for item in copper {
        if item.net.is_empty() {
            continue;
        }
        let in_schematic = match item.kind {
            CopperKind::Via => net_names.contains(&item.net),
            CopperKind::Zone => net_names
                .iter()
                .any(|name| !name.is_empty() && item.net.starts_with(name.as_str())),
        };
        if in_schematic {
            continue;
        }
        let mut to = String::new();
        if item.kind == CopperKind::Zone {
            if let Some(net) = zone_pads
                .get(&item.kiid)
                .into_iter()
                .flatten()
                .map(|(footprint, number)| net_after(footprint, number))
                .find(|net| *net != item.net)
            {
                to = net;
            }
        }
        if to.is_empty() {
            if let Some(net) = old_to_new.get(item.net.as_str()) {
                to = net.to_string();
            }
        }
        if !to.is_empty() {
            plan.copper_net_changes.push(CopperNetChange {
                kind: item.kind,
                kiid: item.kiid.clone(),
                name: item.name.clone(),
                from: item.net.clone(),
                to,
            });
            plan.counts.copper_net_changes.planned += 1;
            continue;
        }
        let (code, message) = match item.kind {
            CopperKind::Via => (
                "via_net_not_in_schematic",
                format!(
                    "via {} is on '{}', which is not in the schematic, and no planned pad moves \
                     off it, so it stays; KiCad's Update PCB warns \"Via connected to unknown \
                     net\"",
                    item.kiid, item.net
                ),
            ),
            CopperKind::Zone => (
                "zone_net_not_in_schematic",
                format!(
                    "copper zone {}({}) is on '{}', which is not in the schematic, and no pad \
                     connected to it or moving off its net takes another, so it stays; KiCad's \
                     Update PCB warns that it has no pads connected",
                    if item.name.is_empty() {
                        String::new()
                    } else {
                        format!("'{}' ", item.name)
                    },
                    item.kiid,
                    item.net
                ),
            ),
        };
        let mut note = conflict(code, message, None);
        note.kiid = Some(item.kiid.clone());
        plan.diagnostics.push(note);
    }
}

/// Each net's routed copper: how many tracks, arcs, vias and copper zones
/// carry it, and whether a zone's net could not be read.
fn read_routed_copper(
    client: &copperpour_ipc::KiCadIpcClient,
    document: &copperpour_ipc::gen::kiapi::common::types::DocumentSpecifier,
) -> Result<(BTreeMap<String, usize>, bool)> {
    use copperpour_ipc::gen::kiapi;
    use kiapi::common::types::KiCadObjectType as ObjectType;

    let mut routed_nets = BTreeMap::new();
    for item in client.get_items_in(document.clone(), ObjectType::KotPcbTrace)? {
        if let Ok(track) = kiapi::board::types::Track::decode(item.value.as_slice()) {
            record_routed_net(&mut routed_nets, track.net.as_ref());
        }
    }
    for item in client.get_items_in(document.clone(), ObjectType::KotPcbArc)? {
        if let Ok(arc) = kiapi::board::types::Arc::decode(item.value.as_slice()) {
            record_routed_net(&mut routed_nets, arc.net.as_ref());
        }
    }
    for item in client.get_items_in(document.clone(), ObjectType::KotPcbVia)? {
        if let Ok(via) = kiapi::board::types::Via::decode(item.value.as_slice()) {
            record_routed_net(&mut routed_nets, via.net.as_ref());
        }
    }
    let mut unreadable_zone = false;
    for item in client.get_items_in(document.clone(), ObjectType::KotPcbZone)? {
        match zone_net(item.value.as_slice()) {
            ZoneNet::Copper(net) => record_routed_net(&mut routed_nets, Some(&net)),
            ZoneNet::RuleArea => {}
            ZoneNet::Unreadable => unreadable_zone = true,
        }
    }
    Ok((routed_nets, unreadable_zone))
}

/// Convert one live KiCad footprint into the planner's board-side view.
///
/// Split out of [`snapshot_board`] so the IPC-to-planner mapping is reachable
/// without a live editor: the planner's own tests build `BoardFootprint`
/// directly and therefore cannot see a defect that lives in this conversion.
fn board_footprint_from_instance(
    footprint: &copperpour_ipc::gen::kiapi::board::types::FootprintInstance,
) -> Result<BoardFootprint> {
    use copperpour_ipc::gen::kiapi;

    let kiid = footprint
        .id
        .as_ref()
        .map(|id| id.value.clone())
        .filter(|id| !id.is_empty())
        .context("KiCad returned a footprint without a KIID")?;
    let definition = footprint
        .definition
        .as_ref()
        .context("KiCad returned a footprint without a definition")?;
    let mut pad_nets = BTreeMap::new();
    let mut pad_numbers = BTreeSet::new();
    for child in &definition.items {
        // Same discriminator as `apply_footprint_fields`, for the same
        // reason: a graphic can decode as an empty pad.
        //
        // No test covers this one, and deliberately so: it has no effect
        // that a message KiCad actually sends can show. Every drawing, field
        // and text in the checked-in KiCad 10 captures fails `Pad::decode` on
        // a wire-type mismatch and is skipped by the `else` below, so
        // neutering this check changes nothing. That was measured again for
        // #657, which added `pad_numbers` to this loop: a shape that did
        // decode would add the empty pad number. It stays so the next person
        // to touch this loop does not have to rediscover why reading
        // `definition.items` untyped is unsafe.
        if !copperpour_ipc::builders::any_is(child, "kiapi.board.types.Pad") {
            continue;
        }
        let Ok(pad) = kiapi::board::types::Pad::decode(child.value.as_slice()) else {
            continue;
        };
        pad_numbers.insert(pad.number.clone());
        if let Some(net) = pad.net.filter(|net| !net.name.is_empty()) {
            pad_nets.insert(pad.number, net.name);
        }
    }
    let mut fields = BTreeMap::new();
    for (name, field) in [
        ("Datasheet", &footprint.datasheet_field),
        ("Description", &footprint.description_field),
    ] {
        if field.is_some() {
            fields.insert(name.to_string(), field_text(field));
        }
    }
    for child in &definition.items {
        if !copperpour_ipc::builders::any_is(child, "kiapi.board.types.Field") {
            continue;
        }
        // A field the snapshot cannot read would plan a second field of the
        // same name, so it refuses, as an unreadable pad refuses an apply.
        let field =
            kiapi::board::types::Field::decode(child.value.as_slice()).with_context(|| {
                format!(
                    "footprint {} ({kiid}) has a field Copperpour cannot read",
                    field_text(&footprint.reference_field)
                )
            })?;
        fields.insert(field.name.clone(), field_text(&Some(field)));
    }
    let position = footprint.position.as_ref();
    Ok(BoardFootprint {
        kiid,
        reference: field_text(&footprint.reference_field),
        value: field_text(&footprint.value_field),
        footprint_id: definition
            .id
            .as_ref()
            .map(|id| format!("{}:{}", id.library_nickname, id.entry_name))
            .unwrap_or_default(),
        symbol_path: board_symbol_path(footprint.symbol_path.as_ref()),
        pad_nets,
        pad_numbers,
        position: Point {
            x: position
                .map(|point| copperpour_ipc::builders::nm_to_mm(point.x_nm))
                .unwrap_or(0.0),
            y: position
                .map(|point| copperpour_ipc::builders::nm_to_mm(point.y_nm))
                .unwrap_or(0.0),
        },
        rotation: footprint
            .orientation
            .as_ref()
            .map(|angle| angle.value_degrees)
            .unwrap_or(0.0),
        layer: board_layer_name(footprint.layer),
        locked: footprint.locked == kiapi::common::types::LockedState::LsLocked as i32,
        dnp: footprint
            .attributes
            .as_ref()
            .map(|attributes| attributes.do_not_populate)
            .unwrap_or(false),
        exclude_from_bom: footprint
            .attributes
            .as_ref()
            .map(|attributes| attributes.exclude_from_bill_of_materials)
            .unwrap_or(false),
        not_in_schematic: footprint
            .attributes
            .as_ref()
            .map(|attributes| attributes.not_in_schematic)
            .unwrap_or(false),
        fields,
        sheet: SymbolSheet::of(footprint),
    })
}

fn field_text(field: &Option<copperpour_ipc::gen::kiapi::board::types::Field>) -> String {
    field
        .as_ref()
        .and_then(|field| field.text.as_ref())
        .and_then(|text| text.text.as_ref())
        .map(|text| text.text.clone())
        .unwrap_or_default()
}

/// The board-side schematic identity of a footprint, or `None` when it has no
/// schematic symbol behind it at all.
///
/// KiCad's IPC layer sends a *present but empty* `SheetPath` for a footprint
/// placed directly on the board — a logo, a fiducial, a mounting hole. Passing
/// that through [`sheet_path_string`] renders `/`, so every such footprint
/// arrives carrying the same synthetic identity and the planner reads them as
/// duplicates of one another, blocking the sync (#452). Absence of an identity
/// must arrive as absence, which is what the rest of this module already
/// assumes `None` to mean.
fn board_symbol_path(
    path: Option<&copperpour_ipc::gen::kiapi::common::types::SheetPath>,
) -> Option<String> {
    let path = path?;
    // A path made only of empty KIIDs names no symbol either, so it is absence
    // too. Only this all-empty case is normalised: an empty segment *inside* an
    // otherwise real path is left to render as it always has, because that is a
    // shape KiCad does not emit and inventing a meaning for it here would be
    // guessing. `apply_footprint_fields` drops empty segments on the way out, so
    // the round trip is exact for every path either side can actually produce.
    if path.path.iter().all(|part| part.value.is_empty()) {
        return None;
    }
    Some(sheet_path_string(path))
}

fn sheet_path_string(path: &copperpour_ipc::gen::kiapi::common::types::SheetPath) -> String {
    format!(
        "/{}",
        path.path
            .iter()
            .map(|part| part.value.as_str())
            .collect::<Vec<_>>()
            .join("/")
    )
}

fn board_layer_name(layer: i32) -> String {
    use copperpour_ipc::gen::kiapi::board::types::BoardLayer;
    match BoardLayer::try_from(layer).ok() {
        Some(BoardLayer::BlFCu) => "F.Cu".to_string(),
        Some(BoardLayer::BlBCu) => "B.Cu".to_string(),
        Some(layer) => layer.as_str_name().to_string(),
        None => format!("layer_{layer}"),
    }
}

/// What a zone pours: a copper zone its own net (`Zone.copper_settings.net`,
/// empty for a zone on no net), a rule area nothing (#779). Shared with
/// `update_footprints_from_library`'s routed-net snapshot (#102).
#[derive(Debug, PartialEq)]
pub(crate) enum ZoneNet {
    Copper(copperpour_ipc::gen::kiapi::board::types::Net),
    RuleArea,
    Unreadable,
}

pub(crate) fn zone_net(bytes: &[u8]) -> ZoneNet {
    use copperpour_ipc::gen::kiapi::board::types::{zone::Settings, Zone};
    match Zone::decode(bytes).map(|zone| zone.settings) {
        Ok(Some(Settings::CopperSettings(copper))) => {
            copper.net.map_or(ZoneNet::Unreadable, ZoneNet::Copper)
        }
        Ok(Some(Settings::RuleAreaSettings(_))) => ZoneNet::RuleArea,
        _ => ZoneNet::Unreadable,
    }
}

fn record_routed_net(
    routed: &mut BTreeMap<String, usize>,
    net: Option<&copperpour_ipc::gen::kiapi::board::types::Net>,
) {
    if let Some(net) = net.filter(|net| !net.name.is_empty()) {
        *routed.entry(net.name.clone()).or_insert(0) += 1;
    }
}

/// A library footprint the plan wants to place and Copperpour could not prepare,
/// with every part that needs it.
#[derive(Debug, Clone, PartialEq, Eq)]
struct UnpreparedFootprint {
    footprint_id: String,
    references: Vec<String>,
    code: &'static str,
    reason: String,
}

impl UnpreparedFootprint {
    fn into_diagnostic(self) -> SyncDiagnostic {
        let message = format!(
            "{} cannot be placed (needed by {}): {}",
            self.footprint_id,
            listed(&self.references),
            self.reason
        );
        footprint_conflict(self.code, message, &self.footprint_id, self.references)
    }
}

/// Read one library footprint into what the typed placement path sends. The
/// three codes are the ones `update_footprints_from_library` uses for the
/// same three failures, because the caller's next step differs: fix the
/// library table, fix the file, or substitute the footprint.
fn prepare_footprint(
    footprint_id: &str,
    board_path: &Path,
) -> std::result::Result<PreparedFootprint, (&'static str, String)> {
    let path = super::pcb_components::resolve_footprint_file(footprint_id, board_path)
        .map_err(|error| ("footprint_library_resolution_failed", format!("{error:#}")))?;
    let source = std::fs::read_to_string(&path).map_err(|error| {
        (
            "footprint_library_read_failed",
            format!("failed to read {}: {error}", path.display()),
        )
    })?;
    prepare_footprint_source(&source)
}

/// [`prepare_footprint`] after the file is read: everything the typed
/// placement path sends, from the library footprint's text.
fn prepare_footprint_source(
    source: &str,
) -> std::result::Result<PreparedFootprint, (&'static str, String)> {
    let unsupported =
        |error: anyhow::Error| ("unsupported_library_footprint", format!("{error:#}"));
    let pads = super::pcb_components::extract_pad_definitions(source).map_err(unsupported)?;
    let fields = super::pcb_components::extract_field_placement(source);
    // Read with the same readers `update_footprints_from_library` uses, so a
    // footprint the sync places carries what a refresh would give it (#789).
    let root = copperpour_sexp::parse_sexp(source)
        .context("invalid footprint S-expression")
        .map_err(unsupported)?;
    let attributes = super::library_footprint::attributes(&root).map_err(unsupported)?;
    let pad_nodes = root.find_all("pad");
    let pad_properties =
        super::library_footprint::pad_properties(&pad_nodes, pads.len()).map_err(unsupported)?;
    let pad_zones = super::library_footprint::pad_zones(&pad_nodes).map_err(unsupported)?;
    let pad_margins =
        super::library_footprint::pad_margins(&root, &pad_nodes).map_err(unsupported)?;
    let margins =
        super::library_footprint::SolderMargins::of_footprint(&root).map_err(unsupported)?;
    let points = super::library_footprint::points(&root).map_err(unsupported)?;
    let net_ties = super::library_footprint::net_ties(&root).map_err(unsupported)?;
    let unsupported_features =
        super::library_footprint::unsupported_features(&root).map_err(unsupported)?;
    let description_and_keywords = super::library_footprint::description_and_keywords(&root);
    let models = super::library_footprint::models(&root).map_err(unsupported)?;
    let library_fields =
        super::library_footprint::properties(&root).map_err(|error| format!("{error:#}"));
    // Properties travel as typed fields when they can be read. Drawing visible
    // ones as plain text as well would show them twice; that drawing is only
    // the fallback for a footprint whose properties could not be read.
    let graphics = if library_fields.is_ok() {
        super::pcb_components::extract_graphic_definitions_without_properties(source)
    } else {
        super::pcb_components::extract_graphic_definitions(source)
    }
    .map_err(unsupported)?;
    let (width, height) = footprint_dimensions(&pads, &graphics);
    Ok(PreparedFootprint {
        pads,
        graphics,
        fields,
        attributes,
        description_and_keywords,
        models,
        library_fields,
        pad_properties,
        pad_zones,
        pad_margins,
        margins,
        points,
        net_ties,
        unsupported: unsupported_features,
        width,
        height,
    })
}

/// Give a footprint built from pads, drawings and fields the rest of what its
/// library carries: the instance attributes and solder margins, the
/// definition's description and keywords, and its 3D models. Without them a
/// synced SMD part had no `smd` attribute, so KiCad treated it as unspecified,
/// and `export_3d`'s default left every one of them out of the STEP (#789).
///
/// Runs before [`apply_footprint_fields`], which sets DNP from the schematic,
/// so the schematic overrides a library `dnp` as KiCad's own update does.
fn apply_library_data(
    footprint: &mut copperpour_ipc::gen::kiapi::board::types::FootprintInstance,
    part: &PreparedFootprint,
) -> Result<()> {
    footprint.attributes = Some(part.attributes.clone());
    super::library_footprint::apply_footprint_margins(footprint, &part.margins);
    let definition = footprint
        .definition
        .as_mut()
        .context("built footprint has no definition")?;
    definition.attributes = Some(part.description_and_keywords.clone());
    definition.items.extend(part.models.iter().map(|model| {
        copperpour_ipc::builders::pack_any(model, "kiapi.board.types.Footprint3DModel")
    }));
    if let Ok(fields) = &part.library_fields {
        apply_library_fields(footprint, fields)?;
    }
    Ok(())
}

/// Give an added footprint its library's fields (#789):
/// - Reference and Value with the library's layer, visibility, font and lock
///   (their text is the schematic's, set by [`apply_footprint_fields`]);
/// - Datasheet and Description where the library has them;
/// - every custom property, such as `KiLib_Generator`.
///
/// They are placed with the library refresh's own transform, so both tools
/// put a library field in the same place. The sync places on the front only.
/// Live placement (`PreparedFootprintPlacement::ipc_item`) gives its parts the
/// same fields.
pub(super) fn apply_library_fields(
    footprint: &mut copperpour_ipc::gen::kiapi::board::types::FootprintInstance,
    fields: &super::library_footprint::LibraryProperties,
) -> Result<()> {
    let position = footprint
        .position
        .context("built footprint has no position")?;
    let rotation = footprint
        .orientation
        .as_ref()
        .map(|angle| angle.value_degrees)
        .unwrap_or(0.0);
    let place = |field: &copperpour_ipc::gen::kiapi::board::types::Field| {
        super::pcb_footprint_update::transform_library_property(field, &position, rotation, None)
    };
    let definition = footprint
        .definition
        .as_mut()
        .context("built footprint has no definition")?;
    if let Some(field) = &fields.reference {
        let placed = place(field)?;
        definition.reference_field = Some(placed.clone());
        footprint.reference_field = Some(placed);
    }
    if let Some(field) = &fields.value {
        let placed = place(field)?;
        definition.value_field = Some(placed.clone());
        footprint.value_field = Some(placed);
    }
    if let Some(field) = &fields.datasheet {
        let placed = place(field)?;
        definition.datasheet_field = Some(placed.clone());
        footprint.datasheet_field = Some(placed);
    }
    if let Some(field) = &fields.description {
        let placed = place(field)?;
        definition.description_field = Some(placed.clone());
        footprint.description_field = Some(placed);
    }
    for field in &fields.custom {
        definition.items.push(copperpour_ipc::builders::pack_any(
            &place(field)?,
            "kiapi.board.types.Field",
        ));
    }
    Ok(())
}

/// Give a footprint its symbol's sheet name, sheet file and footprint filters,
/// as KiCad's Update PCB does (#237).
fn apply_symbol_sheet(
    footprint: &mut copperpour_ipc::gen::kiapi::board::types::FootprintInstance,
    sheet: &SymbolSheet,
) {
    footprint.symbol_sheet_name = sheet.sheet_name.clone();
    footprint.symbol_sheet_filename = sheet.sheet_file.clone();
    footprint.symbol_footprint_filters = sheet.footprint_filters.clone();
}

/// Copy the schematic's fields onto a footprint as KiCad's Update PCB does by
/// default (#788): a field the footprint has takes the symbol's text and keeps
/// its presentation, a field it lacks is added, and a field the symbol lacks
/// stays. Datasheet and Description use their own slots, on the instance and
/// the definition as Reference and Value do; the rest are `Field` items. It
/// runs after [`apply_library_fields`], so the symbol's text wins, as in KiCad.
fn apply_symbol_fields(
    footprint: &mut copperpour_ipc::gen::kiapi::board::types::FootprintInstance,
    fields: &[SymbolField],
) -> Result<()> {
    use copperpour_ipc::gen::kiapi;

    if fields.is_empty() {
        return Ok(());
    }
    let reference = field_text(&footprint.reference_field);
    let position = footprint
        .position
        .with_context(|| format!("footprint {reference} has no position"))?;
    // KiCad turns an added field with its footprint and normalises the angle.
    let rotation = footprint
        .orientation
        .as_ref()
        .map(|angle| angle.value_degrees)
        .unwrap_or(0.0)
        .rem_euclid(360.0);
    let back = footprint.layer == kiapi::board::types::BoardLayer::BlBCu as i32;
    let definition = footprint
        .definition
        .as_mut()
        .with_context(|| format!("footprint {reference} has no library definition"))?;
    'fields: for field in fields {
        let slots = match field.name.as_str() {
            "Datasheet" => Some((
                &mut footprint.datasheet_field,
                &mut definition.datasheet_field,
            )),
            "Description" => Some((
                &mut footprint.description_field,
                &mut definition.description_field,
            )),
            _ => None,
        };
        if let Some((instance, library)) = slots {
            match instance {
                Some(_) => set_field_text(instance, &field.name, &field.value),
                None => *instance = Some(mandatory_symbol_field(field, position, rotation, back)),
            }
            *library = instance.clone();
            continue;
        }
        for child in &mut definition.items {
            if !copperpour_ipc::builders::any_is(child, "kiapi.board.types.Field") {
                continue;
            }
            let mut existing = kiapi::board::types::Field::decode(child.value.as_slice())
                .with_context(|| {
                    format!("footprint {reference} has a field Copperpour cannot read")
                })?;
            if existing.name != field.name {
                continue;
            }
            let board_text = existing.text.get_or_insert_with(Default::default);
            board_text.text.get_or_insert_with(Default::default).text = field.value.clone();
            *child = copperpour_ipc::builders::pack_any(&existing, "kiapi.board.types.Field");
            continue 'fields;
        }
        definition.items.push(copperpour_ipc::builders::pack_any(
            &added_symbol_field(field, position, rotation, back),
            "kiapi.board.types.Field",
        ));
    }
    Ok(())
}

/// The field KiCad's Update PCB adds for a symbol field the footprint lacks:
/// hidden, on F.Fab or B.Fab, at the footprint's position and turned with it,
/// styled as the board's fabrication text and mirrored on the back. Copperpour
/// does not read the board's text settings, so this is KiCad's default for
/// them: 1.0 mm text, 0.15 mm stroke, upright and italic off.
fn added_symbol_field(
    field: &SymbolField,
    position: copperpour_ipc::gen::kiapi::common::types::Vector2,
    rotation: f64,
    back: bool,
) -> copperpour_ipc::gen::kiapi::board::types::Field {
    let layer = if back { "B.Fab" } else { "F.Fab" };
    let text =
        copperpour_ipc::builders::board_text(layer, &field.value, 0.0, 0.0, 1.0, rotation, back);
    hidden_field(field, text, position)
}

/// Datasheet or Description for a footprint whose library has neither. Update
/// PCB only sets their text, since every footprint is constructed with them:
/// hidden on F.Fab, 1.27 mm, kept upright. That is KiCad's default; with Board
/// Setup's "Apply board defaults to footprint fields" on, KiCad restyles them,
/// and Copperpour reads neither that option nor the board's text settings.
fn mandatory_symbol_field(
    field: &SymbolField,
    position: copperpour_ipc::gen::kiapi::common::types::Vector2,
    rotation: f64,
    back: bool,
) -> copperpour_ipc::gen::kiapi::board::types::Field {
    let layer = if back { "B.Fab" } else { "F.Fab" };
    let mut text = copperpour_ipc::builders::board_text_with_stroke_width(
        layer,
        &field.value,
        0.0,
        0.0,
        1.27,
        0.0,
        rotation,
        back,
    );
    if let Some(attributes) = text.text.as_mut().and_then(|text| text.attributes.as_mut()) {
        attributes.keep_upright = true;
    }
    hidden_field(field, text, position)
}

fn hidden_field(
    field: &SymbolField,
    mut text: copperpour_ipc::gen::kiapi::board::types::BoardText,
    position: copperpour_ipc::gen::kiapi::common::types::Vector2,
) -> copperpour_ipc::gen::kiapi::board::types::Field {
    // `board_text` places in millimetres; the footprint's position is kept to
    // the nanometre. A field KiCad makes allows several lines, as its text
    // attributes do by default; `board_text` is for board text and does not.
    if let Some(text) = text.text.as_mut() {
        text.position = Some(position);
        if let Some(attributes) = text.attributes.as_mut() {
            attributes.multiline = true;
        }
    }
    copperpour_ipc::gen::kiapi::board::types::Field {
        id: None,
        name: field.name.clone(),
        text: Some(text),
        visible: false,
    }
}

/// Footprints the plan adds whose library properties could not be read.
/// They are placed with the fields every footprint gets, and the plan says so
/// rather than refusing the part over presentation metadata (#789).
fn library_fields_not_carried(
    plan: &SyncPlan,
    prepared: &BTreeMap<String, PreparedFootprint>,
) -> Vec<SyncDiagnostic> {
    additions_by_footprint(plan)
        .into_iter()
        .filter_map(|(footprint_id, references)| {
            let reason = prepared.get(footprint_id)?.library_fields.as_ref().err()?;
            Some(footprint_conflict(
                "library_fields_not_carried",
                format!(
                    "{footprint_id}'s library properties could not be read, so {} will be \
                     placed with default fields, not the library's: {reason}",
                    listed(&references)
                ),
                footprint_id,
                references,
            ))
        })
        .collect()
}

/// Footprints the plan adds that carry library clauses or children the typed
/// placement path leaves out.
fn unsupported_features_not_carried(
    plan: &SyncPlan,
    prepared: &BTreeMap<String, PreparedFootprint>,
) -> Vec<SyncDiagnostic> {
    additions_by_footprint(plan)
        .into_iter()
        .filter_map(|(footprint_id, references)| {
            let part = prepared.get(footprint_id)?;
            (!part.unsupported.is_empty()).then(|| {
                footprint_conflict(
                    "not_carried",
                    format!(
                        "{footprint_id} has library features not carried by typed placement ({}), \
                         so {} will be placed without them.",
                        part.unsupported.join("; "),
                        listed(&references)
                    ),
                    footprint_id,
                    references,
                )
            })
        })
        .collect()
}

/// Footprints the plan adds whose pads have fabrication properties or reference
/// points a KiCad before 10.0.7 cannot take over its API (#149, #318). They
/// are placed without them, as KiCad's own Update Footprints from Library can
/// still restore, rather than refusing every board with a QFN exposed pad or a
/// BGA.
fn kicad_10_0_7_not_carried(
    plan: &SyncPlan,
    prepared: &BTreeMap<String, PreparedFootprint>,
    kicad: &copperpour_ipc::IpcKiCadVersion,
) -> Vec<SyncDiagnostic> {
    additions_by_footprint(plan)
        .into_iter()
        .filter_map(|(footprint_id, references)| {
            let part = prepared.get(footprint_id)?;
            let reasons = super::library_footprint::not_carried_10_0_7_reasons(
                &part.pad_properties,
                &part.pads,
                &part.points,
                kicad,
            );
            (!reasons.is_empty()).then(|| {
                footprint_conflict(
                    "not_carried",
                    format!(
                        "{footprint_id} ({}): {}; {} will be placed without them. KiCad's \
                         Tools > Update Footprints from Library restores them.",
                        listed(&references),
                        reasons.join("; "),
                        listed(&references)
                    ),
                    footprint_id,
                    references,
                )
            })
        })
        .collect()
}

/// Footprints the plan adds with net-tie pad groups a KiCad before 10.0.1
/// cannot keep over its API ([`carries_net_ties`], #85). They are placed
/// without them, as pad fabrication properties are before 10.0.7, rather
/// than with groups KiCad would read as one pad each.
fn net_ties_not_carried(
    plan: &SyncPlan,
    prepared: &BTreeMap<String, PreparedFootprint>,
    kicad: &copperpour_ipc::IpcKiCadVersion,
) -> Vec<SyncDiagnostic> {
    additions_by_footprint(plan)
        .into_iter()
        .filter_map(|(footprint_id, references)| {
            let part = prepared.get(footprint_id)?;
            let reason =
                super::library_footprint::net_ties_not_carried_reason(&part.net_ties, kicad)?;
            Some(footprint_conflict(
                "not_carried",
                format!(
                    "{footprint_id} ({}): {reason}; {} will be placed without them. KiCad's \
                     Tools > Update Footprints from Library restores them.",
                    listed(&references),
                    listed(&references)
                ),
                footprint_id,
                references,
            ))
        })
        .collect()
}

/// Pad fabrication properties for a message, one entry per pad number and
/// property, bounded as [`listed`] bounds references.
fn listed_pad_properties<'a>(
    pads: impl IntoIterator<Item = (&'a str, PadFabricationProperty)>,
) -> String {
    let mut counts = BTreeMap::<(&str, &str), usize>::new();
    for (number, property) in pads {
        *counts.entry((number, property.as_str())).or_default() += 1;
    }
    let entries = counts
        .into_iter()
        .map(|((number, property), count)| match count {
            1 => format!("pad {number}: {property}"),
            _ => format!("pad {number}: {property} ({count} pads)"),
        })
        .collect::<Vec<_>>();
    listed(&entries)
}

/// The entries of sorted `a` that sorted `b` lacks, counting repeats.
fn sorted_difference<'a>(
    a: &'a [(String, PadFabricationProperty)],
    b: &[(String, PadFabricationProperty)],
) -> Vec<(&'a str, PadFabricationProperty)> {
    let mut b = b.iter().peekable();
    a.iter()
        .filter(|entry| {
            while b.next_if(|other| *other < *entry).is_some() {}
            b.next_if(|other| *other == *entry).is_none()
        })
        .map(|(number, property)| (number.as_str(), *property))
        .collect()
}

/// Every part the plan adds, grouped by library footprint.
fn additions_by_footprint(plan: &SyncPlan) -> BTreeMap<&str, Vec<String>> {
    let mut references: BTreeMap<&str, Vec<String>> = BTreeMap::new();
    for change in &plan.changes {
        if let PlannedChange::Add {
            footprint_id,
            reference,
            ..
        } = change
        {
            references
                .entry(footprint_id.as_str())
                .or_default()
                .push(reference.clone());
        }
    }
    references
}

/// Prepare every footprint the plan adds. One that cannot be prepared does
/// not stop the rest: stopping at the first produced a single diagnostic that
/// named neither the footprint nor a part, and hid every other unusable
/// footprint behind it (#657).
fn prepare_additions(
    board_path: &Path,
    plan: &SyncPlan,
) -> (
    BTreeMap<String, PreparedFootprint>,
    Vec<UnpreparedFootprint>,
) {
    let mut prepared = BTreeMap::new();
    let mut unprepared: BTreeMap<String, UnpreparedFootprint> = BTreeMap::new();
    for change in &plan.changes {
        let PlannedChange::Add {
            footprint_id,
            reference,
            ..
        } = change
        else {
            continue;
        };
        if prepared.contains_key(footprint_id) {
            continue;
        }
        if let Some(failed) = unprepared.get_mut(footprint_id) {
            failed.references.push(reference.clone());
            continue;
        }
        match prepare_footprint(footprint_id, board_path) {
            Ok(part) => {
                prepared.insert(footprint_id.clone(), part);
            }
            Err((code, reason)) => {
                unprepared.insert(
                    footprint_id.clone(),
                    UnpreparedFootprint {
                        footprint_id: footprint_id.clone(),
                        references: vec![reference.clone()],
                        code,
                        reason,
                    },
                );
            }
        }
    }
    (prepared, unprepared.into_values().collect())
}

/// Read and check every footprint a planned replacement rebuilds from, so
/// `ready` means the apply can build each one. A replacement is refused when
/// its new footprint does not resolve, cannot be read, or cannot be rebuilt
/// losslessly (the codes `prepare_footprint` uses), when it needs fields this
/// KiCad's API does not carry, when it lacks a pad the schematic connects, or
/// when it would drop a pad that carries a net on the board now.
fn prepare_replacements(
    board_path: &Path,
    plan: &SyncPlan,
    board: &BoardState,
    kicad: &copperpour_ipc::IpcKiCadVersion,
) -> (
    BTreeMap<String, super::pcb_footprint_update::ReplacementFootprint>,
    Vec<SyncDiagnostic>,
) {
    let mut prepared = BTreeMap::new();
    let mut unusable = BTreeMap::<String, (&'static str, String)>::new();
    let mut diagnostics = Vec::new();
    for change in &plan.changes {
        let PlannedChange::Update {
            kiid,
            reference,
            pad_nets,
            unconnected_pins,
            replace_footprint: Some(replace),
            ..
        } = change
        else {
            continue;
        };
        if !prepared.contains_key(&replace.to) && !unusable.contains_key(&replace.to) {
            match read_replacement(&replace.to, board_path, kicad) {
                Ok(footprint) => {
                    prepared.insert(replace.to.clone(), footprint);
                }
                Err(refusal) => {
                    unusable.insert(replace.to.clone(), refusal);
                }
            }
        }
        if let Some((code, reason)) = unusable.get(&replace.to) {
            diagnostics.push(footprint_conflict(
                code,
                format!("{reference} cannot be replaced by {}: {reason}", replace.to),
                &replace.to,
                vec![reference.clone()],
            ));
            continue;
        }
        let available = prepared[&replace.to].pad_numbers();
        let pad_nets = nets_for_pads(pad_nets, unconnected_pins, &available);
        let missing = missing_pads(&pad_nets, &available);
        if !missing.is_empty() {
            diagnostics.push(pad_missing_conflict(reference, &replace.to, &missing));
            continue;
        }
        let dropped = board
            .footprints
            .iter()
            .find(|footprint| &footprint.kiid == kiid)
            .map(|footprint| {
                footprint
                    .pad_nets
                    .iter()
                    .filter(|(number, net)| !net.is_empty() && !available.contains(*number))
                    .map(|(number, _)| number.clone())
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
        if !dropped.is_empty() {
            diagnostics.push(footprint_conflict(
                "footprint_replacement_drops_connected_pad",
                format!(
                    "replacing {reference}'s {} by {} would drop pad(s) {} that carry a net \
                     on the board",
                    replace.from,
                    replace.to,
                    dropped.join(", ")
                ),
                &replace.to,
                vec![reference.clone()],
            ));
        }
    }
    (prepared, diagnostics)
}

/// Each planned replacement built once against the live footprint, as the
/// apply builds it, so a footprint the builder refuses makes the dry run a
/// conflict instead of failing the apply after a `ready`. Replacements whose
/// footprint was not read are refused already and skipped here.
fn replacements_that_cannot_be_built(
    plan: &SyncPlan,
    replacements: &BTreeMap<String, super::pcb_footprint_update::ReplacementFootprint>,
    copper_layers: u32,
    snapshot: &LiveSnapshot,
) -> Vec<SyncDiagnostic> {
    let mut diagnostics = Vec::new();
    for change in &plan.changes {
        let PlannedChange::Update {
            kiid,
            reference,
            replace_footprint: Some(replace),
            ..
        } = change
        else {
            continue;
        };
        let Some(library) = replacements.get(&replace.to) else {
            continue;
        };
        let built = snapshot
            .items
            .get(kiid)
            .with_context(|| format!("planned footprint {kiid} disappeared"))
            .and_then(|item| {
                copperpour_ipc::gen::kiapi::board::types::FootprintInstance::decode(
                    item.value.as_slice(),
                )
                .context("KiCad returned an invalid footprint item")
            })
            .and_then(|current| {
                super::pcb_footprint_update::replace_footprint(
                    &current,
                    library,
                    copper_layers,
                    &snapshot.net_codes,
                )
            });
        if let Err(error) = built {
            diagnostics.push(footprint_conflict(
                "unsupported_library_footprint",
                format!(
                    "{reference} cannot be replaced by {}: {error:#}",
                    replace.to
                ),
                &replace.to,
                vec![reference.clone()],
            ));
        }
    }
    diagnostics
}

/// One replacement footprint, read as [`prepare_footprint`] reads one, or
/// the code and reason it is refused for.
fn read_replacement(
    footprint_id: &str,
    board_path: &Path,
    kicad: &copperpour_ipc::IpcKiCadVersion,
) -> std::result::Result<super::pcb_footprint_update::ReplacementFootprint, (&'static str, String)>
{
    let path = super::pcb_components::resolve_footprint_file(footprint_id, board_path)
        .map_err(|error| ("footprint_library_resolution_failed", format!("{error:#}")))?;
    let source = std::fs::read_to_string(&path).map_err(|error| {
        (
            "footprint_library_read_failed",
            format!("failed to read {}: {error}", path.display()),
        )
    })?;
    let footprint = super::pcb_footprint_update::ReplacementFootprint::parse(footprint_id, &source)
        .map_err(|error| ("unsupported_library_footprint", format!("{error:#}")))?;
    // An older KiCad would drop these on apply, as the refresh refuses it.
    if let Some(what) = footprint
        .needs_kicad_10_0_7()
        .filter(|_| !super::library_footprint::carries_kicad_10_0_7_fields(kicad))
    {
        return Err((
            "unsupported_library_footprint",
            format!(
                "it has {what}, which KiCad's API carries only from KiCad 10.0.7; this KiCad is {}",
                kicad.full_version
            ),
        ));
    }
    // KiCad 10.0.0 would keep each group as one pad, as the refresh refuses it.
    if footprint.has_net_ties() && !carries_net_ties(kicad) {
        return Err((
            "unsupported_library_footprint",
            format!(
                "it has net-tie pad groups, which KiCad's API carries only from KiCad 10.0.1; \
                 this KiCad is {}",
                kicad.full_version
            ),
        ));
    }
    Ok(footprint)
}

/// Additions whose schematic connects a pad the prepared library footprint
/// does not have. The update path is checked in `plan_sync`, which holds the
/// live footprint's pads; this is the same rule against the library's.
fn additions_missing_pads(
    plan: &SyncPlan,
    prepared: &BTreeMap<String, PreparedFootprint>,
) -> Vec<SyncDiagnostic> {
    let mut diagnostics = Vec::new();
    for change in &plan.changes {
        let PlannedChange::Add {
            reference,
            footprint_id,
            pad_nets,
            unconnected_pins,
            ..
        } = change
        else {
            continue;
        };
        // An unprepared footprint is already reported, with this part named.
        let Some(part) = prepared.get(footprint_id) else {
            continue;
        };
        let available = library_pad_numbers(part);
        let pad_nets = nets_for_pads(pad_nets, unconnected_pins, &available);
        let missing = missing_pads(&pad_nets, &available);
        if !missing.is_empty() {
            diagnostics.push(pad_missing_conflict(reference, footprint_id, &missing));
        }
    }
    diagnostics
}

fn library_pad_numbers(part: &PreparedFootprint) -> BTreeSet<String> {
    part.pads.iter().map(|pad| pad.number.clone()).collect()
}

/// Leave out of each addition's and replacement's pad nets the pins KiCad
/// left unconnected that its library footprint has no pad for
/// (`nets_for_pads`), as `plan_sync` leaves them out of an update, so the
/// apply sets the net of each pad the new footprint has and of no other.
/// An addition or replacement whose footprint was not read is skipped
/// already, and left as it is. The pads reassigned are counted again, as
/// `skip_parts` counts them, since `plan_sync` counted a replacement's
/// against every pin.
fn leave_out_unconnected_pins(
    plan: &mut SyncPlan,
    prepared: &BTreeMap<String, PreparedFootprint>,
    replacements: &BTreeMap<String, super::pcb_footprint_update::ReplacementFootprint>,
    board: &BoardState,
) {
    for change in &mut plan.changes {
        let (pad_nets, unconnected_pins, available) = match change {
            PlannedChange::Add {
                footprint_id,
                pad_nets,
                unconnected_pins,
                ..
            } => match prepared.get(footprint_id) {
                Some(part) => (pad_nets, unconnected_pins, library_pad_numbers(part)),
                None => continue,
            },
            PlannedChange::Update {
                replace_footprint: Some(replace),
                pad_nets,
                unconnected_pins,
                ..
            } => match replacements.get(&replace.to) {
                Some(library) => (pad_nets, unconnected_pins, library.pad_numbers()),
                None => continue,
            },
            PlannedChange::Update { .. } | PlannedChange::Delete { .. } => continue,
        };
        *pad_nets = nets_for_pads(pad_nets, unconnected_pins, &available);
    }
    plan.counts.pads_reassigned.planned = plan
        .changes
        .iter()
        .map(|change| match change {
            PlannedChange::Update { kiid, pad_nets, .. } => board
                .footprints
                .iter()
                .find(|footprint| &footprint.kiid == kiid)
                .map_or(0, |footprint| {
                    changed_pad_count(pad_nets, &footprint.pad_nets)
                }),
            PlannedChange::Add { .. } | PlannedChange::Delete { .. } => 0,
        })
        .sum();
}

/// What a footprint owns that KiCad's API cannot carry through an update (#836).
#[derive(Debug, Default, PartialEq, Eq)]
struct NotCarried {
    groups: usize,
    points: usize,
    variants: Vec<String>,
}

impl NotCarried {
    fn of(footprint: &SexpNode) -> Self {
        NotCarried {
            groups: footprint.find_all("group").len(),
            points: footprint.find_all("point").len(),
            variants: footprint
                .find_all("variant")
                .into_iter()
                // KiCad always writes a variant's name.
                .map(|variant| variant.find_str("name").unwrap_or_default().to_string())
                .collect(),
        }
    }

    fn described(&self) -> String {
        fn counted(count: usize, noun: &str) -> Option<String> {
            match count {
                0 => None,
                1 => Some(format!("1 {noun}")),
                n => Some(format!("{n} {noun}s")),
            }
        }
        let variants = match self.variants.as_slice() {
            [] => None,
            [only] => Some(format!("variant '{only}'")),
            all => Some(format!(
                "variants {}",
                listed(
                    &all.iter()
                        .map(|name| format!("'{name}'"))
                        .collect::<Vec<_>>()
                )
            )),
        };
        let mut parts = [
            counted(self.groups, "group"),
            counted(self.points, "point"),
            variants,
        ]
        .into_iter()
        .flatten()
        .collect::<Vec<_>>();
        let last = parts.pop().unwrap_or_default();
        if parts.is_empty() {
            last
        } else {
            format!("{} and {last}", parts.join(", "))
        }
    }
}

/// Every footprint in a board KiCad wrote, by KIID, with what it owns that an
/// update would drop. A KIID written twice owns what both footprints own.
fn not_carried_by_kiid(board_text: &str) -> Result<BTreeMap<String, NotCarried>> {
    let tree =
        copperpour_sexp::parse_sexp(board_text).context("the board KiCad wrote did not parse")?;
    let mut owned = BTreeMap::<String, NotCarried>::new();
    for footprint in copperpour_sexp::board::footprints(&tree) {
        let Some(kiid) = footprint.find_str("uuid") else {
            continue;
        };
        let found = NotCarried::of(footprint);
        let entry = owned.entry(kiid.to_string()).or_default();
        entry.groups += found.groups;
        entry.points += found.points;
        entry.variants.extend(found.variants);
    }
    Ok(owned)
}

/// Each updated footprint that owns groups, points or variants (#836): the
/// conflicts, and the updates that would set only a footprint's sheet, to be
/// left out with a diagnostic that refuses nothing (#237), by KIID.
///
/// KiCad's API updates a footprint by replacing it with a new one built from
/// the `FootprintInstance` sent (`handleCreateUpdateItemsInternal`, KiCad
/// 10.0.6), and that message carries none of these (`FOOTPRINT::Serialize`).
/// No item read lists them, so `board_text` is the board as KiCad writes it.
fn updates_not_carried(
    plan: &SyncPlan,
    footprints: &[BoardFootprint],
    board_text: &str,
) -> UpdatesNotCarried {
    let owned = not_carried_by_kiid(board_text);
    let footprint_id = |kiid: &str| {
        // The plan was made from these footprints, so the update's is among them.
        footprints
            .iter()
            .find(|footprint| footprint.kiid == kiid)
            .map_or("", |footprint| footprint.footprint_id.as_str())
    };
    let mut diagnostics = Vec::new();
    let mut left_out = Vec::new();
    let mut unknown = false;
    for change in &plan.changes {
        let PlannedChange::Update {
            kiid,
            reference,
            sheet_only,
            ..
        } = change
        else {
            continue;
        };
        let found = owned.as_ref().map(|owned| owned.get(kiid));
        unknown |= !matches!(found, Ok(Some(_)));
        let message = match found {
            Ok(Some(found)) if *found == NotCarried::default() => continue,
            Ok(Some(found)) if *sheet_only => {
                left_out.push((
                    kiid.clone(),
                    footprint_conflict(
                        "footprint_sheet_not_updated",
                        format!(
                            "{reference} differs from the schematic only in its sheet name, \
                             sheet file or footprint filters, and owns {}, which KiCad drops \
                             when its API updates a footprint; it is left out of this sync, \
                             and KiCad's Update PCB from Schematic sets its sheet in place",
                            found.described()
                        ),
                        footprint_id(kiid),
                        vec![reference.clone()],
                    ),
                ));
                continue;
            }
            Ok(Some(found)) => format!(
                "{reference} owns {}, which KiCad drops when its API updates a footprint; \
                 update {reference} with KiCad's Update PCB from Schematic, which edits it \
                 in place",
                found.described()
            ),
            Ok(None) => format!(
                "{reference} ({kiid}) is not in the board KiCad wrote, so what an update \
                 would drop is unknown; run the dry run again"
            ),
            Err(error) => format!("what an update of {reference} would drop is unknown: {error:#}"),
        };
        diagnostics.push(footprint_conflict(
            "footprint_children_not_carried",
            message,
            footprint_id(kiid),
            vec![reference.clone()],
        ));
    }
    UpdatesNotCarried {
        refused: diagnostics,
        left_out,
        unknown,
    }
}

/// What [`updates_not_carried`] found in the board KiCad wrote.
struct UpdatesNotCarried {
    /// A `footprint_children_not_carried` conflict per update it refuses.
    refused: Vec<SyncDiagnostic>,
    /// The sheet-only updates to leave out, by KIID.
    left_out: Vec<(String, SyncDiagnostic)>,
    /// The board text could not be read, or lacks a footprint the plan
    /// updates. What an update would drop is then unknown for a reason that
    /// is not the part's, so the plan is refused whole rather than skipping
    /// each update.
    unknown: bool,
}

/// Take each update [`updates_not_carried`] leaves out off the plan, with its
/// diagnostic; a plan left with no change is a no-op.
fn leave_out_updates(plan: &mut SyncPlan, left_out: Vec<(String, SyncDiagnostic)>) {
    for (kiid, diagnostic) in left_out {
        let planned = plan.changes.len();
        plan.changes.retain(
            |change| !matches!(change, PlannedChange::Update { kiid: planned, .. } if *planned == kiid),
        );
        // `skip_parts` may have taken it off already.
        if plan.changes.len() < planned {
            plan.counts.updated.planned -= 1;
            plan.diagnostics.push(diagnostic);
        }
    }
    if plan.changes.is_empty() && plan.status == PlanStatus::Ready {
        plan.status = PlanStatus::Noop;
    }
}

/// Take the parts these preflight diagnostics name off the plan and report
/// them in `skipped`, so one footprint that cannot be built or updated no
/// longer stops the sync of every other part. KiCad's Update PCB does the
/// same: a part it cannot add or replace is reported as an error and the rest
/// is committed (`BOARD_NETLIST_UPDATER`, KiCad 10.0.8).
///
/// Only the named parts are left out: a net a skipped part shares still
/// reaches every other part's pads. The plan is refused whole instead, as
/// before, and `false` returned, when a diagnostic names no part, or when
/// nothing but the skipped parts and the updates in `left_out` (which
/// `leave_out_updates` takes off next) would be applied: a no-op would hide
/// that the schematic asked for changes.
fn skip_parts(
    plan: &mut SyncPlan,
    diagnostics: Vec<SyncDiagnostic>,
    board: &BoardState,
    left_out: &BTreeSet<&str>,
) -> bool {
    let skipped: BTreeSet<String> = diagnostics
        .iter()
        .flat_map(|diagnostic| diagnostic.references.iter().cloned())
        .collect();
    let applies_something = plan.changes.iter().any(|change| match change {
        PlannedChange::Add { reference, .. } => !skipped.contains(reference),
        PlannedChange::Update {
            kiid, reference, ..
        } => !skipped.contains(reference) && !left_out.contains(kiid.as_str()),
        // A footprint with no symbol is no part the preflight skips (#861).
        PlannedChange::Delete { .. } => true,
    });
    let names_its_parts = diagnostics
        .iter()
        .all(|diagnostic| !diagnostic.references.is_empty());
    if !applies_something || !names_its_parts {
        refuse_plan(plan, diagnostics);
        return false;
    }
    plan.changes.retain(|change| match change {
        PlannedChange::Add { reference, .. } | PlannedChange::Update { reference, .. } => {
            !skipped.contains(reference)
        }
        PlannedChange::Delete { .. } => true,
    });
    // A note about a skipped part (a locked footprint kept, say) no longer
    // describes anything this sync does.
    plan.diagnostics.retain(|note| {
        note.references.is_empty()
            || !note
                .references
                .iter()
                .all(|reference| skipped.contains(reference))
    });
    plan.counts.added.planned = 0;
    plan.counts.updated.planned = 0;
    plan.counts.pads_reassigned.planned = 0;
    for change in &plan.changes {
        match change {
            PlannedChange::Add { .. } => plan.counts.added.planned += 1,
            PlannedChange::Update { kiid, pad_nets, .. } => {
                plan.counts.updated.planned += 1;
                plan.counts.pads_reassigned.planned += board
                    .footprints
                    .iter()
                    .find(|footprint| &footprint.kiid == kiid)
                    .map_or(0, |footprint| {
                        changed_pad_count(pad_nets, &footprint.pad_nets)
                    });
            }
            PlannedChange::Delete { .. } => {}
        }
    }
    plan.counts.skipped_footprint.planned += skipped.len();
    plan.skipped.extend(diagnostics);
    true
}

/// How many pads change net, as `plan_sync` counts them for an update,
/// including a routed change the caller acknowledged (#38).
fn changed_pad_count(new: &BTreeMap<String, String>, old: &BTreeMap<String, String>) -> usize {
    new.keys()
        .chain(old.keys())
        .collect::<BTreeSet<_>>()
        .into_iter()
        .filter(|number| {
            new.get(*number).map_or("", String::as_str)
                != old.get(*number).map_or("", String::as_str)
        })
        .count()
}

/// Turn a plan into a conflict the way `plan_sync` does for its own
/// diagnostics: nothing stays planned, so nothing can be applied.
fn refuse_plan(plan: &mut SyncPlan, diagnostics: Vec<SyncDiagnostic>) {
    plan.status = PlanStatus::Conflict;
    plan.counts.added.planned = 0;
    plan.counts.updated.planned = 0;
    plan.counts.pads_reassigned.planned = 0;
    plan.counts.conflicts.planned += diagnostics.len();
    plan.diagnostics.extend(diagnostics);
    clear_changes(plan);
}

/// Take every change off a refused plan. A footprint it would have deleted
/// stays on the board, so it is listed and counted with those kept (#861).
fn clear_changes(plan: &mut SyncPlan) {
    for change in plan.changes.drain(..) {
        if let PlannedChange::Delete {
            kiid,
            reference,
            footprint_id,
            reason,
        } = change
        {
            plan.without_symbols.push(FootprintWithoutSymbol {
                reference,
                kiid,
                footprint_id,
                // Only an unlocked footprint is planned for deletion.
                locked: false,
                reason,
            });
            plan.counts.board_only_preserved.planned += 1;
        }
    }
    plan.counts.deleted.planned = 0;
    plan.copper_net_changes.clear();
    plan.counts.copper_net_changes.planned = 0;
}

/// The pads connected to copper zones, read for `plan_copper_nets` (#862).
struct ZonePads {
    /// For each copper zone whose net is not in the schematic, the footprint
    /// pads KiCad's connectivity joins to it, as `(footprint kiid, pad
    /// number)`, sorted.
    pads: BTreeMap<String, Vec<(String, String)>>,
    /// The KiCad holding the board cannot list them (before 10.0.1) and a
    /// zone needed them.
    unread: bool,
    kicad: String,
}

/// Read the pads connected to each copper zone whose net is not in the
/// schematic, when the plan changes a pad so a zone could follow one.
///
/// KiCad's Update PCB caches the pads each zone touches
/// (`CONNECTIVITY_DATA::GetConnectedPads`); the API gives the zone's whole
/// copper cluster on its net (`GetConnectedItems`, KiCad 10.0.1+), which adds
/// the pads a track joins to the zone. Both are on the zone's net before the
/// update, so they differ only when that net splits, where KiCad's own choice
/// follows the order its pads sit in memory.
fn connected_zone_pads(
    client: &copperpour_ipc::KiCadIpcClient,
    snapshot: &LiveSnapshot,
    plan: &SyncPlan,
    net_names: &BTreeSet<String>,
) -> Result<ZonePads> {
    use copperpour_ipc::gen::kiapi;

    let mut found = ZonePads {
        pads: BTreeMap::new(),
        unread: false,
        kicad: String::new(),
    };
    let changes_a_pad = plan
        .changes
        .iter()
        .any(|change| matches!(change, PlannedChange::Update { .. }));
    if plan.status == PlanStatus::Conflict || !changes_a_pad {
        return Ok(found);
    }
    let zones = snapshot
        .copper
        .iter()
        .filter(|item| item.kind == CopperKind::Zone && !item.net.is_empty())
        .filter(|item| {
            !net_names
                .iter()
                .any(|name| !name.is_empty() && item.net.starts_with(name.as_str()))
        })
        .collect::<Vec<_>>();
    if zones.is_empty() {
        return Ok(found);
    }
    let kicad = client.get_kicad_version()?;
    if !super::library_footprint::carries_net_ties(&kicad) {
        // `GetConnectedItems` came with 10.0.1, as net-tie pad groups did.
        found.unread = true;
        found.kicad = format!("{}.{}.{}", kicad.major, kicad.minor, kicad.patch);
        return Ok(found);
    }
    for zone in zones {
        let mut pads = client
            .get_connected_items_in(
                snapshot.document.clone(),
                &zone.kiid,
                &[kiapi::common::types::KiCadObjectType::KotPcbPad],
            )?
            .iter()
            .filter_map(|item| kiapi::board::types::Pad::decode(item.value.as_slice()).ok())
            .filter_map(|pad| {
                pad.id
                    .and_then(|id| snapshot.pad_owners.get(&id.value).cloned())
            })
            .collect::<Vec<_>>();
        pads.sort();
        pads.dedup();
        found.pads.insert(zone.kiid.clone(), pads);
    }
    Ok(found)
}

/// Each via and copper zone the plan moves, as `UpdateItems` receives it:
/// the item KiCad listed with only its net changed. KiCad finds the net by
/// name, so a net the commit's pads bring is found too
/// (`BOARD_CONNECTED_ITEM::UnpackNet`).
fn copper_net_updates(plan: &SyncPlan, snapshot: &LiveSnapshot) -> Result<Vec<prost_types::Any>> {
    use copperpour_ipc::gen::kiapi::board::types::{zone::Settings, Net, NetCode, Via, Zone};

    plan.copper_net_changes
        .iter()
        .map(|change| {
            let item = snapshot.copper_items.get(&change.kiid).with_context(|| {
                format!("planned {:?} {} disappeared", change.kind, change.kiid)
            })?;
            let net = Net {
                code: snapshot
                    .net_codes
                    .get(&change.to)
                    .copied()
                    .map(|value| NetCode { value }),
                name: change.to.clone(),
            };
            Ok(match change.kind {
                CopperKind::Via => {
                    let mut via = Via::decode(item.value.as_slice())
                        .context("KiCad listed an invalid via")?;
                    via.net = Some(net);
                    copperpour_ipc::builders::pack_any(&via, "kiapi.board.types.Via")
                }
                CopperKind::Zone => {
                    let mut zone = Zone::decode(item.value.as_slice())
                        .context("KiCad listed an invalid zone")?;
                    let Some(Settings::CopperSettings(copper)) = zone.settings.as_mut() else {
                        bail!("planned copper zone {} is not a copper zone", change.kiid);
                    };
                    copper.net = Some(net);
                    copperpour_ipc::builders::pack_any(&zone, "kiapi.board.types.Zone")
                }
            })
        })
        .collect()
}

/// Each moved via or zone KiCad reads back on another net than the plan's,
/// with the net it reads back on (`""` when it is gone).
fn copper_not_moved<'a>(
    client: &copperpour_ipc::KiCadIpcClient,
    document: &copperpour_ipc::gen::kiapi::common::types::DocumentSpecifier,
    moved: &'a [CopperNetChange],
) -> Result<Vec<(&'a CopperNetChange, String)>> {
    if moved.is_empty() {
        return Ok(Vec::new());
    }
    let (copper, _) = read_copper_items(client, document)?;
    Ok(moved
        .iter()
        .filter_map(|change| {
            let net = copper
                .iter()
                .find(|item| item.kiid == change.kiid)
                .map(|item| item.net.clone())
                .unwrap_or_default();
            (net != change.to).then_some((change, net))
        })
        .collect())
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

/// The footprints the plan deletes, as the list of kept footprints would name
/// them: only an unlocked footprint is planned for deletion.
fn planned_deletions(plan: &SyncPlan) -> Vec<FootprintWithoutSymbol> {
    plan.changes
        .iter()
        .filter_map(|change| match change {
            PlannedChange::Delete {
                kiid,
                reference,
                footprint_id,
                reason,
            } => Some(FootprintWithoutSymbol {
                reference: reference.clone(),
                kiid: kiid.clone(),
                footprint_id: footprint_id.clone(),
                locked: false,
                reason: *reason,
            }),
            _ => None,
        })
        .collect()
}

/// Each of `deletions` KiCad still lists after the commit.
fn footprints_still_on_board(
    client: &copperpour_ipc::KiCadIpcClient,
    document: &copperpour_ipc::gen::kiapi::common::types::DocumentSpecifier,
    deletions: &[FootprintWithoutSymbol],
) -> Result<Vec<FootprintWithoutSymbol>> {
    use copperpour_ipc::gen::kiapi;

    if deletions.is_empty() {
        return Ok(Vec::new());
    }
    let listed = client
        .get_items_in(
            document.clone(),
            kiapi::common::types::KiCadObjectType::KotPcbFootprint,
        )?
        .iter()
        .filter_map(|item| {
            kiapi::board::types::FootprintInstance::decode(item.value.as_slice())
                .ok()
                .and_then(|footprint| footprint.id)
                .map(|id| id.value)
        })
        .collect::<BTreeSet<_>>();
    Ok(deletions
        .iter()
        .filter(|footprint| listed.contains(&footprint.kiid))
        .cloned()
        .collect())
}

fn footprint_dimensions(
    pads: &[copperpour_ipc::IpcPadDefinition],
    graphics: &[copperpour_ipc::IpcGraphicDefinition],
) -> (f64, f64) {
    use copperpour_ipc::IpcGraphicDefinition as Graphic;

    let (mut min_x, mut min_y) = (f64::INFINITY, f64::INFINITY);
    let (mut max_x, mut max_y) = (f64::NEG_INFINITY, f64::NEG_INFINITY);
    let mut include = |x: f64, y: f64| {
        min_x = min_x.min(x);
        min_y = min_y.min(y);
        max_x = max_x.max(x);
        max_y = max_y.max(y);
    };
    for pad in pads {
        include(pad.x - pad.size_x / 2.0, pad.y - pad.size_y / 2.0);
        include(pad.x + pad.size_x / 2.0, pad.y + pad.size_y / 2.0);
    }
    for graphic in graphics {
        match graphic {
            Graphic::Line { start, end, .. } | Graphic::Rect { start, end, .. } => {
                include(start.0, start.1);
                include(end.0, end.1);
            }
            Graphic::Circle { center, end, .. } => {
                let radius = ((end.0 - center.0).powi(2) + (end.1 - center.1).powi(2)).sqrt();
                include(center.0 - radius, center.1 - radius);
                include(center.0 + radius, center.1 + radius);
            }
            Graphic::Arc {
                start, mid, end, ..
            } => {
                include(start.0, start.1);
                include(mid.0, mid.1);
                include(end.0, end.1);
            }
            Graphic::Poly { points, .. } => {
                for point in points {
                    include(point.0, point.1);
                }
            }
            Graphic::Text { position, size, .. } => {
                include(position.0 - size / 2.0, position.1 - size / 2.0);
                include(position.0 + size / 2.0, position.1 + size / 2.0);
            }
        }
    }
    if !min_x.is_finite() {
        return (10.0, 10.0);
    }
    ((max_x - min_x).max(1.0), (max_y - min_y).max(1.0))
}

fn restage_additions(
    plan: &mut SyncPlan,
    prepared: &BTreeMap<String, PreparedFootprint>,
    bounds: Bounds,
) {
    let mut next_y = bounds.min_y;
    for change in &mut plan.changes {
        let PlannedChange::Add {
            footprint_id,
            position,
            ..
        } = change
        else {
            continue;
        };
        let dimensions = prepared.get(footprint_id);
        let width = dimensions.map(|part| part.width).unwrap_or(10.0);
        let height = dimensions.map(|part| part.height).unwrap_or(10.0);
        *position = Point {
            x: bounds.max_x + 5.0 + width / 2.0,
            y: next_y + height / 2.0,
        };
        next_y += height + 5.0;
    }
}

/// One footprint the plan adds, as `CreateItems` receives it: built from the
/// library's pads, drawings and fields, given the rest of the library's data,
/// then the schematic's reference, value, symbol path, DNP and pad nets.
/// `carried` says what the KiCad it goes to takes of pad fabrication
/// properties and reference points (10.0.7+) and net-tie pad groups (10.0.1+).
fn build_added_footprint(
    change: &PlannedChange,
    part: &PreparedFootprint,
    net_codes: &BTreeMap<String, i32>,
    carried: CarriedByKiCad,
) -> Result<prost_types::Any> {
    let PlannedChange::Add {
        reference,
        value,
        footprint_id,
        symbol_path,
        dnp,
        exclude_from_bom,
        pad_nets,
        position,
        fields,
        sheet,
        ..
    } = change
    else {
        bail!("only a planned addition builds a new footprint");
    };
    let item = copperpour_ipc::KiCadIpcClient::build_footprint_item(
        footprint_id,
        reference,
        value,
        &part.pads,
        &part.graphics,
        &part.fields,
        position.x,
        position.y,
        0.0,
        "F.Cu",
    )?;
    let mut footprint =
        copperpour_ipc::gen::kiapi::board::types::FootprintInstance::decode(item.value.as_slice())?;
    let footprint_pos = footprint
        .position
        .context("built footprint has no position")?;
    let definition = footprint
        .definition
        .as_mut()
        .context("built footprint has no definition")?;
    super::library_footprint::apply_pad_settings(
        definition,
        &part.pad_properties,
        &part.pad_zones,
        &part.pad_margins,
        carried.fields_10_0_7,
    )?;
    if carried.fields_10_0_7 {
        super::library_footprint::apply_reference_points(
            definition,
            &part.points,
            &footprint_pos,
            0.0,
            None,
        )?;
    }
    if carried.net_ties {
        definition.net_ties = part.net_ties.clone();
    }
    apply_library_data(&mut footprint, part)?;
    apply_symbol_fields(&mut footprint, fields)?;
    apply_symbol_sheet(&mut footprint, sheet);
    apply_footprint_fields(
        &mut footprint,
        reference,
        value,
        symbol_path,
        *dnp,
        *exclude_from_bom,
        pad_nets,
        net_codes,
    )?;
    Ok(copperpour_ipc::builders::pack_any(
        &footprint,
        "kiapi.board.types.FootprintInstance",
    ))
}

fn build_mutation_items(
    plan: &SyncPlan,
    prepared: &BTreeMap<String, PreparedFootprint>,
    replacements: &BTreeMap<String, super::pcb_footprint_update::ReplacementFootprint>,
    copper_layers: u32,
    snapshot: &LiveSnapshot,
    carried: CarriedByKiCad,
) -> Result<(Vec<prost_types::Any>, Vec<prost_types::Any>)> {
    let mut creates = Vec::new();
    let mut updates = Vec::new();
    for change in &plan.changes {
        match change {
            PlannedChange::Add { footprint_id, .. } => {
                let part = prepared
                    .get(footprint_id)
                    .with_context(|| format!("no prepared footprint for {footprint_id}"))?;
                creates.push(build_added_footprint(
                    change,
                    part,
                    &snapshot.net_codes,
                    carried,
                )?);
            }
            PlannedChange::Update {
                kiid,
                replace_footprint,
                ..
            } => {
                let item = snapshot
                    .items
                    .get(kiid)
                    .with_context(|| format!("planned footprint {kiid} disappeared"))?;
                let replaced;
                let item = match replace_footprint {
                    Some(replace) => {
                        let library = replacements.get(&replace.to).with_context(|| {
                            format!("no prepared footprint {} for the replacement", replace.to)
                        })?;
                        let current =
                            copperpour_ipc::gen::kiapi::board::types::FootprintInstance::decode(
                                item.value.as_slice(),
                            )
                            .context("KiCad returned an invalid footprint item")?;
                        replaced = copperpour_ipc::builders::pack_any(
                            &super::pcb_footprint_update::replace_footprint(
                                &current,
                                library,
                                copper_layers,
                                &snapshot.net_codes,
                            )?,
                            "kiapi.board.types.FootprintInstance",
                        );
                        &replaced
                    }
                    None => item,
                };
                updates.push(update_footprint_item(item, change, &snapshot.net_codes)?);
            }
            // Sent by the handler, in the same commit (`planned_deletions`).
            PlannedChange::Delete { .. } => {}
        }
    }
    Ok((creates, updates))
}

#[cfg(test)]
mod chunk_tests {
    use super::*;
    use crate::test_support::MockIpcServer;
    use copperpour_ipc::{builders::pack_any, gen::kiapi, KiCadIpcClient};
    use std::sync::{Arc, Mutex};

    fn footprints(count: usize) -> Vec<prost_types::Any> {
        let source = include_str!("../../tests/fixtures/c_0603_1608metric_kicad10.kicad_mod");
        let pads = super::super::pcb_components::extract_pad_definitions(source).unwrap();
        let graphics = super::super::pcb_components::extract_graphic_definitions(source).unwrap();
        let fields = super::super::pcb_components::extract_field_placement(source);
        (0..count)
            .map(|i| {
                KiCadIpcClient::build_footprint_item(
                    "Capacitor_SMD:C_0603_1608Metric",
                    &format!("C{}", i + 1),
                    "100n",
                    &pads,
                    &graphics,
                    &fields,
                    5.0 + (i % 10) as f64 * 4.0,
                    5.0 + (i / 10) as f64 * 4.0,
                    0.0,
                    "F.Cu",
                )
                .unwrap()
            })
            .collect()
    }

    #[derive(Default)]
    struct Observed {
        chunks: Vec<usize>,
        references: Vec<String>,
        staged: usize,
        begins: usize,
        actions: Vec<i32>,
        updates: usize,
    }

    fn exercise(count: usize, refusal: bool, malformed: bool) -> (Result<()>, Observed) {
        let directory = tempfile::tempdir().unwrap();
        let board = directory.path().join("chunk.kicad_pcb");
        let document =
            super::super::pcb_board::board_mock::board_document(&board.to_string_lossy());
        let target = document.clone();
        let state = Arc::new(Mutex::new(Observed::default()));
        let observed = state.clone();
        let mock = MockIpcServer::spawn("sync-chunks", move |request| {
            let command = request.message.unwrap();
            let mut state = observed.lock().unwrap();
            let mut response = kiapi::common::ApiResponse {
                status: Some(kiapi::common::ApiResponseStatus {
                    status: kiapi::common::ApiStatusCode::AsOk as i32,
                    error_message: String::new(),
                }),
                ..Default::default()
            };
            response.message = match command.type_url.rsplit('.').next().unwrap() {
                "GetOpenDocuments" => Some(pack_any(
                    &kiapi::common::commands::GetOpenDocumentsResponse {
                        documents: vec![target.clone()],
                    },
                    "kiapi.common.commands.GetOpenDocumentsResponse",
                )),
                "SaveDocumentToString" => {
                    let request = kiapi::common::commands::SaveDocumentToString::decode(
                        command.value.as_slice(),
                    )
                    .unwrap();
                    assert_eq!(request.document, Some(target.clone()));
                    Some(pack_any(
                        &kiapi::common::commands::SavedDocumentResponse {
                            document: Some(target.clone()),
                            contents: include_str!(
                                "../../../copperpour-sexp/tests/fixtures/gr_poly_outline.kicad_pcb"
                            )
                            .into(),
                        },
                        "kiapi.common.commands.SavedDocumentResponse",
                    ))
                }
                "BeginCommit" => {
                    state.begins += 1;
                    Some(pack_any(
                        &kiapi::common::commands::BeginCommitResponse {
                            id: Some(kiapi::common::types::Kiid {
                                value: "chunk-commit".into(),
                            }),
                        },
                        "kiapi.common.commands.BeginCommitResponse",
                    ))
                }
                "CreateItems" => {
                    let request =
                        kiapi::common::commands::CreateItems::decode(command.value.as_slice())
                            .unwrap();
                    assert_eq!(request.header.unwrap().document, Some(target.clone()));
                    state.chunks.push(request.items.len());
                    for item in &request.items {
                        let fp =
                            kiapi::board::types::FootprintInstance::decode(item.value.as_slice())
                                .unwrap();
                        state
                            .references
                            .push(fp.reference_field.unwrap().text.unwrap().text.unwrap().text);
                    }
                    state.staged += request.items.len();
                    if state.chunks.len() == 2 && refusal {
                        response.status.as_mut().unwrap().status =
                            kiapi::common::ApiStatusCode::AsBadRequest as i32;
                        response.status.as_mut().unwrap().error_message =
                            "injected second-chunk refusal".into();
                        None
                    } else if state.chunks.len() == 2 && malformed {
                        response.status = None;
                        None
                    } else {
                        Some(pack_any(
                            &kiapi::common::commands::CreateItemsResponse {
                                header: None,
                                status: kiapi::common::types::ItemRequestStatus::IrsOk as i32,
                                created_items: request
                                    .items
                                    .into_iter()
                                    .map(|item| kiapi::common::commands::ItemCreationResult {
                                        status: Some(kiapi::common::commands::ItemStatus {
                                            code: kiapi::common::commands::ItemStatusCode::IscOk
                                                as i32,
                                            error_message: String::new(),
                                        }),
                                        item: Some(item),
                                    })
                                    .collect(),
                            },
                            "kiapi.common.commands.CreateItemsResponse",
                        ))
                    }
                }
                "UpdateItems" => {
                    state.updates += 1;
                    let request =
                        kiapi::common::commands::UpdateItems::decode(command.value.as_slice())
                            .unwrap();
                    assert_eq!(request.header.unwrap().document, Some(target.clone()));
                    Some(pack_any(
                        &kiapi::common::commands::UpdateItemsResponse {
                            header: None,
                            status: kiapi::common::types::ItemRequestStatus::IrsOk as i32,
                            updated_items: request
                                .items
                                .into_iter()
                                .map(|item| kiapi::common::commands::ItemUpdateResult {
                                    status: Some(kiapi::common::commands::ItemStatus {
                                        code: kiapi::common::commands::ItemStatusCode::IscOk as i32,
                                        error_message: String::new(),
                                    }),
                                    item: Some(item),
                                })
                                .collect(),
                        },
                        "kiapi.common.commands.UpdateItemsResponse",
                    ))
                }
                "EndCommit" => {
                    let request =
                        kiapi::common::commands::EndCommit::decode(command.value.as_slice())
                            .unwrap();
                    assert_eq!(request.id.unwrap().value, "chunk-commit");
                    state.actions.push(request.action);
                    if request.action == kiapi::common::commands::CommitAction::CmaDrop as i32 {
                        state.staged = 0;
                    }
                    Some(pack_any(
                        &kiapi::common::commands::EndCommitResponse {},
                        "kiapi.common.commands.EndCommitResponse",
                    ))
                }
                other => panic!("unexpected {other}"),
            };
            response
        });
        let items = footprints(count);
        let result = KiCadIpcClient::new(mock.address()).run_commit_recovering_in(
            document.clone(),
            "sync",
            |client| {
                create_sync_items_in(client, &document, &items)?;
                client.update_items_in(document.clone(), footprints(1))?;
                Ok(())
            },
        );
        drop(mock);
        let state = Arc::try_unwrap(state).ok().unwrap().into_inner().unwrap();
        (result, state)
    }

    #[test]
    fn sync_chunk_boundaries_preserve_order_and_one_commit() {
        for (count, expected) in [
            (0, vec![]),
            (1, vec![1]),
            (32, vec![32]),
            (33, vec![32, 1]),
            (70, vec![32, 32, 6]),
        ] {
            let (result, state) = exercise(count, false, false);
            assert!(result.is_ok(), "{result:?}");
            assert_eq!(state.chunks, expected);
            assert_eq!(
                state.references,
                (1..=count).map(|i| format!("C{i}")).collect::<Vec<_>>()
            );
            assert_eq!(state.begins, 1);
            assert_eq!(
                state.actions,
                vec![kiapi::common::commands::CommitAction::CmaCommit as i32]
            );
            assert_eq!(state.staged, count);
            assert_eq!(state.updates, 1);
        }
    }

    #[test]
    fn sync_chunk_failure_stops_and_drops_without_publishing_partial_work() {
        let (result, state) = exercise(70, true, false);
        assert!(result.unwrap_err().to_string().contains("changes dropped"));
        assert_eq!(state.chunks, vec![32, 32]);
        assert_eq!(
            state.actions,
            vec![kiapi::common::commands::CommitAction::CmaDrop as i32]
        );
        assert_eq!(state.staged, 0);
        assert_eq!(state.updates, 0);
    }

    #[test]
    fn sync_chunk_uncertainty_stops_without_blind_drop_or_publish() {
        let (result, state) = exercise(70, false, true);
        assert!(matches!(
            copperpour_ipc::IpcFailure::from_error(result.unwrap_err()),
            copperpour_ipc::IpcFailure::Uncertain(_)
        ));
        assert_eq!(state.chunks, vec![32, 32]);
        assert!(state.actions.is_empty());
        assert_eq!(state.staged, 64);
        assert_eq!(state.updates, 0);
    }

    // RunAction is a version-specific acceptance probe, not a supported tool API.
    fn native_action(address: &str, action: &str) -> Result<()> {
        use nng::options::Options;
        let socket = nng::Socket::new(nng::Protocol::Req0)?;
        socket.set_opt::<nng::options::SendTimeout>(Some(std::time::Duration::from_secs(5)))?;
        socket.set_opt::<nng::options::RecvTimeout>(Some(std::time::Duration::from_secs(5)))?;
        socket.dial(address)?;
        let request = kiapi::common::ApiRequest {
            header: Some(kiapi::common::ApiRequestHeader {
                kicad_token: std::env::var("KICAD_API_TOKEN").unwrap_or_default(),
                client_name: "copperpour-chunk-acceptance".into(),
            }),
            message: Some(pack_any(
                &kiapi::common::commands::RunAction {
                    action: action.into(),
                },
                "kiapi.common.commands.RunAction",
            )),
        };
        socket
            .send(request.encode_to_vec().as_slice())
            .map_err(|(_, error)| error)?;
        let response = socket.recv()?;
        let response = kiapi::common::ApiResponse::decode(response.as_slice())?;
        anyhow::ensure!(
            response.status.context("missing action status")?.status
                == kiapi::common::ApiStatusCode::AsOk as i32,
            "action API refusal"
        );
        let message = response.message.context("missing action response")?;
        anyhow::ensure!(
            message
                .type_url
                .ends_with("kiapi.common.commands.RunActionResponse"),
            "unexpected action response"
        );
        let response =
            kiapi::common::commands::RunActionResponse::decode(message.value.as_slice())?;
        anyhow::ensure!(
            response.status == kiapi::common::commands::RunActionStatus::RasOk as i32,
            "native action not submitted"
        );
        Ok(())
    }

    #[test]
    #[ignore = "requires COPPERPOUR_LIVE_CHUNK_BOARD: disposable sole open PCB and KiCad 10.0.6 API"]
    fn sync_chunk_live_one_undo_restores_entire_board() {
        let board = std::path::PathBuf::from(
            crate::app_dirs::env_var("LIVE_CHUNK_BOARD").expect("disposable board required"),
        );
        let address = std::env::var("KICAD_API_SOCKET")
            .ok()
            .or_else(copperpour_ipc::detect_ipc_address)
            .expect("KiCad IPC required");
        let client = KiCadIpcClient::new(address.clone());
        let document = client.find_open_board(&board).unwrap();
        assert_eq!(
            client.get_open_documents().unwrap(),
            vec![document.clone()],
            "actions require sole disposable board"
        );
        let baseline = client.save_document_to_string_in(document.clone()).unwrap();
        let before = client
            .get_items_in(
                document.clone(),
                kiapi::common::types::KiCadObjectType::KotPcbFootprint,
            )
            .unwrap();
        let items = footprints(70);
        client
            .run_commit_recovering_in(
                document.clone(),
                "70-footprint sync chunk acceptance",
                |client| create_sync_items_in(client, &document, &items),
            )
            .unwrap();
        let after = client
            .get_items_in(
                document.clone(),
                kiapi::common::types::KiCadObjectType::KotPcbFootprint,
            )
            .unwrap();
        assert_eq!(after.len(), before.len() + 70);
        let mut references = after
            .iter()
            .map(|item| {
                kiapi::board::types::FootprintInstance::decode(item.value.as_slice())
                    .unwrap()
                    .reference_field
                    .unwrap()
                    .text
                    .unwrap()
                    .text
                    .unwrap()
                    .text
            })
            .collect::<Vec<_>>();
        references.sort();
        for i in 1..=70 {
            assert!(references.contains(&format!("C{i}")));
        }
        let published = client.save_document_to_string_in(document.clone()).unwrap();
        assert_ne!(published, baseline);
        let wait_for = |expected: &str| {
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
            loop {
                if client.save_document_to_string_in(document.clone()).unwrap() == expected {
                    break;
                }
                assert!(
                    std::time::Instant::now() < deadline,
                    "native action did not restore entire serialized board"
                );
                std::thread::sleep(std::time::Duration::from_millis(50));
            }
        };
        native_action(&address, "common.Interactive.undo").unwrap();
        wait_for(&baseline);
        native_action(&address, "common.Interactive.redo").unwrap();
        wait_for(&published);
        native_action(&address, "common.Interactive.undo").unwrap();
        wait_for(&baseline);
        eprintln!("LIVE PASS: 70 footprints in 32/32/6 chunks; one Undo restores full baseline; Redo restores full published state; final Undo leaves baseline.");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const ONE_RESISTOR: &str = r#"
(export
  (components
    (comp
      (ref "R1")
      (value "10k")
      (footprint "Resistor_SMD:R_0603_1608Metric")
      (sheetpath (names "/Power/") (tstamps "/sheet-uuid/"))
      (tstamps "symbol-uuid")
      (units (unit (name "A") (pins (pin (num "1")) (pin (num "2")))))))
  (nets
    (net (code "1") (name "/Power/VCC") (class "Default")
      (node (ref "R1") (pin "1") (pintype "passive")))
    (net (code "2") (name "GND") (class "Default")
      (node (ref "R1") (pin "2") (pintype "passive")))))
"#;

    #[test]
    fn exported_netlist_is_one_flattened_source_of_component_and_pad_truth() {
        let design = parse_exported_netlist(ONE_RESISTOR).expect("valid KiCad netlist");

        assert_eq!(design.components.len(), 1);
        let component = &design.components[0];
        assert_eq!(component.reference, "R1");
        assert_eq!(component.value, "10k");
        assert_eq!(component.footprint_id, "Resistor_SMD:R_0603_1608Metric");
        assert_eq!(component.symbol_path, "/sheet-uuid/symbol-uuid");
        assert_eq!(
            component.pad_nets.get("1").map(String::as_str),
            Some("/Power/VCC")
        );
        assert_eq!(component.pad_nets.get("2").map(String::as_str), Some("GND"));
        assert!(!component.dnp);
    }

    /// Real `kicad-cli sch export netlist` output (KiCad 10.0.8): an LM339
    /// with three of its units placed, and a keyed header whose pin 2, at the
    /// key, has a no-connect flag. Provenance in
    /// `tests/fixtures/pcb_sync_multi_unit_keyed_kicad10/README.md`.
    const MULTI_UNIT_KEYED: &str =
        include_str!("../../tests/fixtures/pcb_sync_multi_unit_keyed_kicad10/kk_sync.net");

    fn exported<'a>(design: &'a ExportedDesign, reference: &str) -> &'a DesignComponent {
        design
            .components
            .iter()
            .find(|component| component.reference == reference)
            .unwrap_or_else(|| panic!("{reference} is exported"))
    }

    /// Every unit's UUID is read, in the order KiCad lists them, the first
    /// unit's path being the one an added footprint is linked to. Only the
    /// first was read before, so a footprint linked to another unit matched
    /// nothing.
    #[test]
    fn every_unit_of_a_multi_unit_symbol_is_read_from_the_netlist() {
        let design = parse_exported_netlist(MULTI_UNIT_KEYED).expect("valid KiCad netlist");

        let u1 = exported(&design, "U1");
        assert_eq!(u1.symbol_path, "/d6286f98-cc91-4832-97c0-cec9c712e8d3");
        assert_eq!(
            u1.other_unit_paths,
            [
                "/ea566a33-6106-497d-ab3a-228e365858d7",
                "/1951944c-c6bb-40ae-8bca-507fb207334e",
            ]
        );
        assert_eq!(
            exported(&design, "R1").other_unit_paths,
            Vec::<String>::new()
        );
    }

    /// The pins KiCad left unconnected: the header's pin 2, which has a
    /// no-connect flag, its pin 4, wired to nothing, and every unwired pin of
    /// the comparator. Each is alone on an `unconnected-…` net.
    #[test]
    fn the_pins_kicad_left_unconnected_are_read_from_the_netlist() {
        let design = parse_exported_netlist(MULTI_UNIT_KEYED).expect("valid KiCad netlist");

        let pins = |reference| {
            exported(&design, reference)
                .unconnected_pins
                .iter()
                .map(String::as_str)
                .collect::<Vec<_>>()
        };
        assert_eq!(pins("P1"), ["2", "4"]);
        assert_eq!(pins("R1"), Vec::<&str>::new());
        assert_eq!(pins("U1"), ["1", "12", "3", "5", "6", "7"]);
        // They still carry their nets, for the footprints that have them.
        assert_eq!(
            exported(&design, "P1")
                .pad_nets
                .get("2")
                .map(String::as_str),
            Some("unconnected-(P1-Pin_2-Pad2)")
        );
    }

    /// A pin with a no-connect flag that is wired to another pin all the same
    /// is connected, and so is a pin alone on a net a label names: only a pin
    /// alone on KiCad's own `unconnected-…` net, or with a no-connect pin
    /// type, connects nothing.
    #[test]
    fn a_no_connect_pin_wired_to_another_pin_is_still_connected() {
        let design = parse_exported_netlist(
            r#"(export
  (components
    (comp (ref "P1") (value "Conn") (footprint "kk:KK254_1x04_Keyed")
      (sheetpath (names "/") (tstamps "/")) (tstamps "p1-uuid"))
    (comp (ref "R1") (value "1k") (footprint "Resistor_SMD:R_0603_1608Metric")
      (sheetpath (names "/") (tstamps "/")) (tstamps "r1-uuid")))
  (nets
    (net (code "1") (name "Net-(P1-Pin_2)") (class "Default")
      (node (ref "P1") (pin "2") (pintype "passive+no_connect"))
      (node (ref "R1") (pin "1") (pintype "passive")))
    (net (code "2") (name "TEST_POINT") (class "Default")
      (node (ref "R1") (pin "2") (pintype "passive")))
    (net (code "3") (name "Net-(P1-Pin_3)") (class "Default")
      (node (ref "P1") (pin "3") (pintype "no_connect")))))
"#,
        )
        .expect("valid netlist");

        assert_eq!(
            exported(&design, "P1").unconnected_pins,
            BTreeSet::from(["3".to_string()])
        );
        assert_eq!(exported(&design, "R1").unconnected_pins, BTreeSet::new());
    }

    /// Real `kicad-cli sch export netlist` output: one sheet file placed
    /// twice, as `CH1` and `CH2`, with a capacitor on it, and a capacitor on
    /// the root. Provenance in `tests/fixtures/sheet_fields_kicad10.README.md`.
    const SHEET_FIELDS: &str = include_str!("../../tests/fixtures/sheet_fields_kicad10.net");

    /// #237: each footprint gets its sheet path's names, the `Sheetfile`
    /// property and the `ki_fp_filters` property, as KiCad's Update PCB gives
    /// them. The names win over the `Sheetname` property: the root's is
    /// `sheet_fields`, and its footprint's sheet name is `/`.
    #[test]
    fn each_component_carries_its_sheet_as_update_pcb_reads_it() {
        let design = parse_exported_netlist(SHEET_FIELDS).unwrap();

        let sheets = design
            .components
            .iter()
            .map(|component| {
                (
                    component.reference.as_str(),
                    component.sheet.sheet_name.as_str(),
                    component.sheet.sheet_file.as_str(),
                    component.sheet.footprint_filters.as_str(),
                )
            })
            .collect::<Vec<_>>();
        assert_eq!(
            sheets,
            vec![
                ("C1", "/", "sheet_fields.kicad_sch", "C_*"),
                ("C2", "/CH1/", "channel.kicad_sch", "C_*"),
                ("C3", "/CH2/", "channel.kicad_sch", "C_*"),
            ]
        );
    }

    /// KiCad 10 writes every sheet path's names, so the fixture is edited to
    /// reach the case KiCad's reader also covers: no names, and the sheet name
    /// is the `Sheetname` property.
    #[test]
    fn a_sheet_path_without_names_takes_the_sheetname_property() {
        let design =
            parse_exported_netlist(&SHEET_FIELDS.replace("(names \"/CH1/\")", "(names \"\")"))
                .unwrap();

        assert_eq!(design.components[1].sheet.sheet_name, "CH1");
        assert_eq!(design.components[2].sheet.sheet_name, "/CH2/");
    }

    /// A property named twice keeps its last value, as in KiCad's reader.
    /// KiCad writes each once, so the fixture is edited to repeat C1's.
    #[test]
    fn a_property_named_twice_keeps_its_last_value() {
        let property = "(name \"ki_fp_filters\")\n\t\t\t\t(value \"C_*\")\n\t\t\t)";
        let repeated = SHEET_FIELDS.replacen(
            property,
            &format!(
                "{property}\n\t\t\t(property\n\t\t\t\t(name \"ki_fp_filters\")\n\t\t\t\t\
                 (value \"CP_*\")\n\t\t\t)"
            ),
            1,
        );
        let design = parse_exported_netlist(&repeated).unwrap();

        assert_eq!(design.components[0].sheet.footprint_filters, "CP_*");
        assert_eq!(design.components[1].sheet.footprint_filters, "C_*");
    }

    /// Real `kicad-cli sch export netlist` output: `R1` never had a footprint
    /// assigned, so the export carries no `(footprint …)` node for it and
    /// still nets its pin 1 to `C1`. Provenance in
    /// `tests/fixtures/unassigned_footprint.README.md`.
    const UNASSIGNED: &str = include_str!("../../tests/fixtures/unassigned_footprint.net");

    /// #507: one footprint-less symbol failed the whole sync with "KiCad
    /// netlist node is missing footprint" — nothing about which component,
    /// every other component blocked with it.
    #[test]
    fn a_component_without_a_footprint_is_reported_not_fatal() {
        let design = parse_exported_netlist(UNASSIGNED).expect("a real export parses");

        let mut placed: Vec<&str> = design
            .components
            .iter()
            .map(|component| component.reference.as_str())
            .collect();
        placed.sort_unstable();
        assert_eq!(placed, ["C1", "R2"]);
        assert_eq!(design.unassigned.len(), 1);
        let unassigned = &design.unassigned[0];
        assert_eq!(unassigned.reference, "R1");
        assert_eq!(unassigned.value, "R");
        assert_eq!(unassigned.lib_id.as_deref(), Some("Device:R"));
        assert!(unassigned.symbol_path.starts_with('/'));
        // Its wired pin names a component with no pads; the net node is
        // dropped, and C1's side of the same net is kept.
        let c1 = design
            .components
            .iter()
            .find(|component| component.reference == "C1")
            .unwrap();
        assert_eq!(
            c1.pad_nets.get("1").map(String::as_str),
            Some("Net-(C1-Pad1)")
        );
    }

    fn symbol_field(name: &str, value: &str) -> SymbolField {
        SymbolField {
            name: name.to_string(),
            value: value.to_string(),
        }
    }

    /// The KiCad-written export carries the fields KiCad's Update PCB copies,
    /// in the order KiCad lists them: the custom ones, Datasheet and
    /// Description, an empty one included, and never Footprint (#788).
    #[test]
    fn a_kicad_export_carries_the_symbol_fields_update_pcb_copies() {
        let design = parse_exported_netlist(include_str!(
            "../../tests/fixtures/symbol_fields_kicad10.net"
        ))
        .expect("a real export parses");

        let fields = |reference: &str| {
            design
                .components
                .iter()
                .find(|component| component.reference == reference)
                .unwrap()
                .fields
                .clone()
        };
        assert_eq!(
            fields("C1"),
            [
                symbol_field("LCSC", "C788001"),
                symbol_field("MPN", "MPN-788-C1"),
                symbol_field("Datasheet", ""),
                symbol_field("Description", "Unpolarized capacitor"),
            ]
        );
        assert_eq!(
            fields("R1"),
            [
                symbol_field("LCSC", "C788002"),
                symbol_field("MPN", ""),
                symbol_field("Datasheet", ""),
                symbol_field("Description", "Resistor"),
            ]
        );
    }

    /// A name the export repeats keeps its first position and its last value,
    /// as KiCad's netlist reader keeps it. Two values for one name would
    /// otherwise make every sync plan the same update again.
    #[test]
    fn a_repeated_field_keeps_its_first_place_and_its_last_value() {
        let source = include_str!("../../tests/fixtures/symbol_fields_kicad10.net");
        let repeated = source.replacen(
            "(name \"Description\") \"Unpolarized capacitor\")",
            "(name \"Description\") \"Unpolarized capacitor\")\n\t\t\t\t(field\n\t\t\t\t\t(name \"LCSC\") \"C788009\")",
            1,
        );
        assert_ne!(repeated, source);

        let design = parse_exported_netlist(&repeated).unwrap();

        let c1 = design
            .components
            .iter()
            .find(|component| component.reference == "C1")
            .unwrap();
        assert_eq!(
            c1.fields,
            [
                symbol_field("LCSC", "C788009"),
                symbol_field("MPN", "MPN-788-C1"),
                symbol_field("Datasheet", ""),
                symbol_field("Description", "Unpolarized capacitor"),
            ]
        );
    }

    fn unassigned_design() -> ExportedDesign {
        parse_exported_netlist(UNASSIGNED).unwrap()
    }

    /// The real export with one `(comp …)` block copied under another
    /// reference's name, placed *after* `after`. kicad-cli cannot produce a
    /// duplicate reference, so the order-sensitive inputs are the real file
    /// with one block duplicated by hand.
    fn with_duplicate_of(copied: &str, renamed_to: &str, after: &str) -> String {
        let block = comp_block(UNASSIGNED, copied).replace(
            &format!("(ref \"{copied}\")"),
            &format!("(ref \"{renamed_to}\")"),
        );
        let anchor = comp_block(UNASSIGNED, after);
        let at = UNASSIGNED.find(anchor).unwrap() + anchor.len();
        format!("{}{}{}", &UNASSIGNED[..at], block, &UNASSIGNED[at..])
    }

    fn comp_block<'a>(source: &'a str, reference: &str) -> &'a str {
        let start = source.find(&format!("(ref \"{reference}\")")).unwrap();
        let start = source[..start].rfind("(comp").unwrap();
        let mut depth = 0usize;
        for (offset, ch) in source[start..].char_indices() {
            match ch {
                '(' => depth += 1,
                ')' => {
                    depth -= 1;
                    if depth == 0 {
                        return &source[start..start + offset + 1];
                    }
                }
                _ => {}
            }
        }
        unreachable!("unbalanced comp block")
    }

    fn refuses_duplicate(source: &str) {
        let error = parse_exported_netlist(source)
            .expect_err("a duplicate reference must refuse before any plan")
            .to_string();
        assert!(error.contains("duplicate component reference"), "{error}");
    }

    /// Order-sensitive: the second `R1` is unassigned like the first.
    #[test]
    fn a_repeated_unassigned_reference_is_refused() {
        refuses_duplicate(&with_duplicate_of("R1", "R1", "R1"));
    }

    /// Unassigned `R1` first, then an assigned component renamed to `R1`.
    #[test]
    fn an_assigned_repeat_of_an_unassigned_reference_is_refused() {
        refuses_duplicate(&with_duplicate_of("R2", "R1", "R1"));
    }

    /// Assigned `C1` first, then an unassigned component renamed to `C1`.
    #[test]
    fn an_unassigned_repeat_of_an_assigned_reference_is_refused() {
        refuses_duplicate(&with_duplicate_of("R1", "C1", "C1"));
    }

    /// Assigned then assigned, the case the parser already refused.
    #[test]
    fn a_repeated_assigned_reference_is_still_refused() {
        refuses_duplicate(&with_duplicate_of("R2", "C1", "R2"));
    }

    /// Nothing asserted the *response* keys — every other test here reads the
    /// `SyncPlan` struct — so a renamed or dropped JSON field was invisible to
    /// the suite. This pins the two names a caller reads.
    #[test]
    fn the_response_names_the_collection_in_the_plural_and_the_count_in_the_singular() {
        let design = unassigned_design();
        let plan = plan_sync(UNASSIGNED, &design, &board_with(vec![]));
        let result = sync_response(&plan, "ready", 1, false, &ResponseOptions::default());
        let text = match result.content.first() {
            Some(crate::mcp::protocol::ToolContent::Text { text }) => text.clone(),
            other => panic!("expected text content, got {other:?}"),
        };
        let value: serde_json::Value = serde_json::from_str(&text).unwrap();

        let unassigned = value["unassigned_footprints"]
            .as_array()
            .expect("the collection is a plural-named array");
        assert_eq!(unassigned.len(), 1);
        assert_eq!(unassigned[0]["reference"], "R1");
        assert_eq!(unassigned[0]["board_state"], "absent");
        // The count category keeps its singular name.
        assert_eq!(value["coverage"]["unassigned_footprint"]["planned"], 1);
        assert!(value.get("unassigned_footprint").is_none());
    }

    /// The helper builds what it claims: the duplicated block parses as a
    /// second component when it is given a fresh reference instead.
    #[test]
    fn the_duplicate_helper_produces_a_parseable_export() {
        let design = parse_exported_netlist(&with_duplicate_of("R2", "R3", "R2")).unwrap();
        assert_eq!(design.components.len(), 3);
        assert_eq!(design.unassigned.len(), 1);
    }

    /// Empty board: the two assigned components are planned as additions, the
    /// unassigned one is listed as absent, and the plan is ready — not a
    /// conflict, and R1 is not counted under `added`.
    #[test]
    fn the_sync_goes_on_for_every_assigned_component() {
        let design = unassigned_design();
        let plan = plan_sync(UNASSIGNED, &design, &board_with(vec![]));

        assert_eq!(plan.status, PlanStatus::Ready);
        assert!(plan.diagnostics.is_empty());
        assert_eq!(plan.counts.added.planned, 2);
        assert_eq!(plan.counts.unassigned_footprint.planned, 1);
        assert_eq!(plan.unassigned.len(), 1);
        assert_eq!(plan.unassigned[0].reference, "R1");
        assert_eq!(plan.unassigned[0].board_state, UnassignedBoardState::Absent);
        assert!(plan.changes.iter().all(|change| {
            !matches!(change, PlannedChange::Add { reference, .. } if reference == "R1")
        }));
    }

    /// A footprint already on the board for the unassigned symbol is kept as
    /// it is and counted as matched — not board-only, not a conflict.
    #[test]
    fn an_existing_board_footprint_for_an_unassigned_symbol_is_kept() {
        let design = unassigned_design();
        let r1_path = design.unassigned[0].symbol_path.clone();
        let plan = plan_sync(
            UNASSIGNED,
            &design,
            &board_with(vec![board_resistor("R1", Some(&r1_path))]),
        );

        assert_eq!(plan.status, PlanStatus::Ready);
        assert!(plan.diagnostics.is_empty());
        assert_eq!(plan.unassigned[0].board_state, UnassignedBoardState::Kept);
        assert_eq!(plan.counts.board_only_preserved.planned, 0);
        assert!(!plan.changes.iter().any(
            |change| matches!(change, PlannedChange::Update { reference, .. } if reference == "R1")
        ));
    }

    /// Nothing but unassigned parts is a no-op that still names them.
    #[test]
    fn an_all_unassigned_schematic_is_a_noop_with_the_list() {
        let mut design = unassigned_design();
        design.components.clear();
        let plan = plan_sync(UNASSIGNED, &design, &board_with(vec![]));

        assert_eq!(plan.status, PlanStatus::Noop);
        assert_eq!(plan.counts.unassigned_footprint.planned, 1);
        assert_eq!(plan.unassigned.len(), 1);
    }

    /// A genuine conflict still clears the changes, but the report about the
    /// schematic survives it.
    #[test]
    fn the_unassigned_list_survives_a_conflict() {
        let design = unassigned_design();
        let mut stranger = board_resistor("C1", Some("/other/identity"));
        stranger.footprint_id = "Capacitor_SMD:C_0603_1608Metric".to_string();
        let plan = plan_sync(UNASSIGNED, &design, &board_with(vec![stranger]));

        assert_eq!(plan.status, PlanStatus::Conflict);
        assert!(plan.changes.is_empty());
        assert_eq!(plan.unassigned.len(), 1);
    }

    fn resistor(reference: &str, symbol_path: &str) -> DesignComponent {
        DesignComponent {
            reference: reference.to_string(),
            value: "10k".to_string(),
            footprint_id: "Resistor_SMD:R_0603_1608Metric".to_string(),
            symbol_path: symbol_path.to_string(),
            other_unit_paths: Vec::new(),
            dnp: false,
            exclude_from_bom: false,
            pad_nets: BTreeMap::from([
                ("1".to_string(), "VCC".to_string()),
                ("2".to_string(), "GND".to_string()),
            ]),
            unconnected_pins: BTreeSet::new(),
            fields: Vec::new(),
            sheet: SymbolSheet::default(),
        }
    }

    fn board_resistor(reference: &str, symbol_path: Option<&str>) -> BoardFootprint {
        BoardFootprint {
            kiid: format!("{reference}-kiid"),
            reference: reference.to_string(),
            value: "10k".to_string(),
            footprint_id: "Resistor_SMD:R_0603_1608Metric".to_string(),
            symbol_path: symbol_path.map(str::to_string),
            pad_nets: BTreeMap::from([
                ("1".to_string(), "VCC".to_string()),
                ("2".to_string(), "GND".to_string()),
            ]),
            pad_numbers: BTreeSet::from(["1".to_string(), "2".to_string()]),
            position: Point { x: 1.0, y: 2.0 },
            rotation: 0.0,
            layer: "F.Cu".to_string(),
            locked: false,
            dnp: false,
            exclude_from_bom: false,
            not_in_schematic: false,
            fields: BTreeMap::new(),
            sheet: SymbolSheet::default(),
        }
    }

    fn board_with(footprints: Vec<BoardFootprint>) -> BoardState {
        BoardState {
            footprints,
            routed_nets: BTreeMap::new(),
            bounds: Bounds {
                min_x: 0.0,
                min_y: 0.0,
                max_x: 10.0,
                max_y: 10.0,
            },
        }
    }

    /// R1 and R2 on the board, both to be updated (R1 with a pad moving to
    /// another net), and R3 and R4 to be added.
    fn two_updates_and_two_additions() -> (SyncPlan, BoardState) {
        let mut r1 = resistor("R1", "/r1");
        r1.value = "4k7".to_string();
        r1.pad_nets.insert("1".to_string(), "VDD".to_string());
        let mut r2 = resistor("R2", "/r2");
        r2.value = "1k".to_string();
        let design = ExportedDesign {
            root_sheet_uuid: None,
            components: vec![r1, r2, resistor("R3", "/r3"), resistor("R4", "/r4")],
            skipped: Vec::new(),
            unassigned: Vec::new(),
            net_names: BTreeSet::new(),
        };
        let board = board_with(vec![
            board_resistor("R1", Some("/r1")),
            board_resistor("R2", Some("/r2")),
        ]);
        let plan = plan_sync("", &design, &board);
        assert_eq!(plan.status, PlanStatus::Ready);
        assert_eq!(plan.counts.added.planned, 2);
        assert_eq!(plan.counts.updated.planned, 2);
        assert_eq!(plan.counts.pads_reassigned.planned, 1);
        (plan, board)
    }

    fn planned_references(plan: &SyncPlan) -> Vec<&str> {
        let mut references: Vec<&str> = plan
            .changes
            .iter()
            .map(|change| match change {
                PlannedChange::Add { reference, .. }
                | PlannedChange::Update { reference, .. }
                | PlannedChange::Delete { reference, .. } => reference.as_str(),
            })
            .collect();
        references.sort_unstable();
        references
    }

    fn cannot_build(code: &str, references: &[&str]) -> SyncDiagnostic {
        footprint_conflict(
            code,
            format!("{} cannot be built", references.join(", ")),
            "Resistor_SMD:R_0603_1608Metric",
            references
                .iter()
                .map(|reference| reference.to_string())
                .collect(),
        )
    }

    /// A part whose footprint cannot be built, and an update that would lose
    /// what its footprint owns, are skipped; every other part stays planned,
    /// the counts follow (R1's net change goes with R1), the skipped parts are
    /// reported with their own diagnostics, and a note about a skipped part
    /// goes while one about a planned part stays.
    #[test]
    fn a_part_that_cannot_be_built_is_skipped_and_the_rest_stays_planned() {
        let (mut plan, board) = two_updates_and_two_additions();
        plan.diagnostics.push(conflict(
            "footprint_locked_not_replaced",
            "R1".to_string(),
            Some("R1"),
        ));
        plan.diagnostics.push(conflict(
            "footprint_locked_not_replaced",
            "R2".to_string(),
            Some("R2"),
        ));
        let preflight = vec![
            cannot_build("unsupported_library_footprint", &["R3"]),
            cannot_build("footprint_children_not_carried", &["R1"]),
        ];

        assert!(skip_parts(
            &mut plan,
            preflight.clone(),
            &board,
            &BTreeSet::new()
        ));

        assert_eq!(plan.status, PlanStatus::Ready);
        assert_eq!(planned_references(&plan), ["R2", "R4"]);
        assert_eq!(plan.counts.added.planned, 1);
        assert_eq!(plan.counts.updated.planned, 1);
        assert_eq!(plan.counts.pads_reassigned.planned, 0);
        assert_eq!(plan.counts.skipped_footprint.planned, 2);
        assert_eq!(plan.counts.conflicts.planned, 0);
        assert_eq!(plan.skipped, preflight);
        let notes: Vec<_> = plan
            .diagnostics
            .iter()
            .map(|note| note.message.as_str())
            .collect();
        assert_eq!(notes, ["R2"]);
    }

    /// Skipping only R2 keeps R1's net change counted.
    #[test]
    fn skipping_a_part_keeps_the_net_changes_of_the_rest() {
        let (mut plan, board) = two_updates_and_two_additions();
        assert!(skip_parts(
            &mut plan,
            vec![cannot_build("footprint_children_not_carried", &["R2"])],
            &board,
            &BTreeSet::new(),
        ));
        assert_eq!(planned_references(&plan), ["R1", "R3", "R4"]);
        assert_eq!(plan.counts.pads_reassigned.planned, 1);
    }

    /// An acknowledged routed pad net change (#38) goes with its part when
    /// the part is skipped: its note is dropped, so the apply neither makes
    /// nor reports it, and the rest of the plan stays.
    #[test]
    fn skipping_a_part_drops_its_acknowledged_net_changes() {
        let (mut design, mut board) = routed_r1_moving_both_pads();
        let mut r2 = resistor("R2", "/r2");
        r2.value = "1k".to_string();
        design.components.push(r2);
        board.footprints.push(board_resistor("R2", Some("/r2")));
        let acknowledged = BTreeSet::from([
            acknowledging("R1", "1", "VCC", "VCC_A"),
            acknowledging("R1", "2", "GND", "GND_A"),
        ]);
        let mut plan = plan_sync_acknowledging("netlist", &design, &board, &acknowledged, false);
        assert_eq!(plan.status, PlanStatus::Ready, "{:?}", plan.diagnostics);
        assert_eq!(plan.counts.pads_reassigned.planned, 2);
        let skipped = vec![cannot_build("unsupported_library_footprint", &["R1"])];

        assert!(skip_parts(
            &mut plan,
            skipped.clone(),
            &board,
            &BTreeSet::new()
        ));

        assert_eq!(plan.status, PlanStatus::Ready);
        assert_eq!(planned_references(&plan), ["R2"]);
        assert_eq!(plan.counts.pads_reassigned.planned, 0);
        assert_eq!(plan.skipped, skipped);
        assert!(
            plan.diagnostics
                .iter()
                .all(|note| note.code != "routed_pad_net_change_acknowledged"),
            "{:?}",
            plan.diagnostics
        );
    }

    /// When every planned part would be skipped there is nothing to apply,
    /// and the plan is refused whole with the diagnostics as conflicts, as it
    /// was before parts were skipped.
    #[test]
    fn skipping_every_part_refuses_the_plan() {
        let (mut plan, board) = two_updates_and_two_additions();
        let all = cannot_build("footprint_pad_missing", &["R1", "R2", "R3", "R4"]);

        assert!(!skip_parts(
            &mut plan,
            vec![all.clone()],
            &board,
            &BTreeSet::new()
        ));

        assert_eq!(plan.status, PlanStatus::Conflict);
        assert!(plan.changes.is_empty());
        assert_eq!(plan.counts.added.planned, 0);
        assert_eq!(plan.counts.updated.planned, 0);
        assert_eq!(plan.counts.conflicts.planned, 1);
        assert_eq!(plan.counts.skipped_footprint.planned, 0);
        assert!(plan.skipped.is_empty());
        assert_eq!(plan.diagnostics, [all]);
    }

    /// An update that `leave_out_updates` takes off next counts as nothing to
    /// apply: a plan left with only that would read `noop` and hide the parts
    /// the schematic asked for. It is refused instead.
    #[test]
    fn a_plan_left_with_only_left_out_updates_is_refused_not_a_noop() {
        let (mut plan, board) = two_updates_and_two_additions();
        let skipped = cannot_build("unsupported_library_footprint", &["R1", "R3", "R4"]);

        assert!(!skip_parts(
            &mut plan,
            vec![skipped],
            &board,
            &BTreeSet::from(["R2-kiid"]),
        ));

        assert_eq!(plan.status, PlanStatus::Conflict);
        assert!(plan.changes.is_empty());
    }

    /// A preflight diagnostic that names no part cannot be skipped by part,
    /// so it refuses the whole plan, the named ones with it.
    #[test]
    fn a_diagnostic_without_parts_refuses_the_whole_plan() {
        let (mut plan, board) = two_updates_and_two_additions();
        let preflight = vec![
            cannot_build("unsupported_library_footprint", &["R3"]),
            conflict("library_unreadable", "no parts".to_string(), None),
        ];

        assert!(!skip_parts(&mut plan, preflight, &board, &BTreeSet::new()));

        assert_eq!(plan.status, PlanStatus::Conflict);
        assert!(plan.changes.is_empty());
        assert_eq!(plan.counts.conflicts.planned, 2);
        assert!(plan.skipped.is_empty());
    }

    /// `leave_out_updates` counts and reports only an update still planned,
    /// so one `skip_parts` already took off is not taken off twice.
    #[test]
    fn leaving_out_a_skipped_update_changes_nothing() {
        let (mut plan, board) = two_updates_and_two_additions();
        assert!(skip_parts(
            &mut plan,
            vec![cannot_build("unsupported_library_footprint", &["R2"])],
            &board,
            &BTreeSet::new(),
        ));
        let diagnostic = cannot_build("footprint_sheet_not_updated", &["R2"]);

        leave_out_updates(&mut plan, vec![("R2-kiid".to_string(), diagnostic)]);

        assert_eq!(plan.counts.updated.planned, 1);
        assert!(plan.diagnostics.is_empty());
        assert_eq!(plan.status, PlanStatus::Ready);
    }

    /// What was skipped, and why, is part of the reviewed plan: the same
    /// changes with another part skipped, or the same part skipped for
    /// another reason, are another plan revision.
    #[test]
    fn the_skipped_parts_are_part_of_the_plan_revision() {
        let revision = |skipped: Vec<SyncDiagnostic>| {
            let (mut plan, board) = two_updates_and_two_additions();
            assert!(skip_parts(&mut plan, skipped, &board, &BTreeSet::new()));
            refresh_revision_with_staging(&mut plan);
            plan.plan_revision
        };
        let unsupported = revision(vec![cannot_build("unsupported_library_footprint", &["R3"])]);
        let unresolved = revision(vec![cannot_build(
            "footprint_library_resolution_failed",
            &["R3"],
        )]);
        assert_ne!(unsupported, unresolved);
        assert_eq!(
            unsupported,
            revision(vec![cannot_build("unsupported_library_footprint", &["R3"])])
        );
    }

    #[test]
    fn planner_matches_identity_preserves_pose_and_stages_new_parts_deterministically() {
        let design = ExportedDesign {
            root_sheet_uuid: None,
            components: vec![
                resistor("R2", "/sheet/existing"),
                resistor("R3", "/sheet/new"),
            ],
            skipped: Vec::new(),
            unassigned: Vec::new(),
            net_names: BTreeSet::new(),
        };
        let board = BoardState {
            footprints: vec![
                BoardFootprint {
                    kiid: "existing-kiid".to_string(),
                    reference: "R1".to_string(),
                    value: "1k".to_string(),
                    footprint_id: "Resistor_SMD:R_0603_1608Metric".to_string(),
                    symbol_path: Some("/sheet/existing".to_string()),
                    pad_nets: BTreeMap::from([
                        ("1".to_string(), "VCC".to_string()),
                        ("2".to_string(), "GND".to_string()),
                    ]),
                    pad_numbers: BTreeSet::from(["1".to_string(), "2".to_string()]),
                    position: Point { x: 25.0, y: 30.0 },
                    rotation: 90.0,
                    layer: "B.Cu".to_string(),
                    locked: true,
                    dnp: false,
                    exclude_from_bom: false,
                    not_in_schematic: false,
                    fields: BTreeMap::new(),
                    sheet: SymbolSheet::default(),
                },
                BoardFootprint {
                    kiid: "board-only".to_string(),
                    reference: "MH1".to_string(),
                    value: "MountingHole".to_string(),
                    footprint_id: "MountingHole:MountingHole_3.2mm_M3".to_string(),
                    symbol_path: None,
                    pad_nets: BTreeMap::new(),
                    pad_numbers: BTreeSet::new(),
                    position: Point { x: 2.0, y: 2.0 },
                    rotation: 0.0,
                    layer: "F.Cu".to_string(),
                    locked: true,
                    dnp: false,
                    exclude_from_bom: false,
                    not_in_schematic: true,
                    fields: BTreeMap::new(),
                    sheet: SymbolSheet::default(),
                },
            ],
            routed_nets: BTreeMap::new(),
            bounds: Bounds {
                min_x: 0.0,
                min_y: 0.0,
                max_x: 50.0,
                max_y: 40.0,
            },
        };

        let first = plan_sync("netlist bytes", &design, &board);
        let second = plan_sync("netlist bytes", &design, &board);

        assert_eq!(first.status, PlanStatus::Ready);
        assert_eq!(first.plan_revision, second.plan_revision);
        assert_eq!(first.counts.added.planned, 1);
        assert_eq!(first.counts.updated.planned, 1);
        assert_eq!(first.counts.board_only_preserved.planned, 1);
        assert_eq!(first.changes, second.changes);
        assert!(first.changes.iter().any(|change| matches!(
            change,
            PlannedChange::Update { kiid, reference, preserve, .. }
                if kiid == "existing-kiid"
                    && reference == "R2"
                    && preserve.position == Point { x: 25.0, y: 30.0 }
                    && preserve.rotation == 90.0
                    && preserve.layer == "B.Cu"
                    && preserve.locked
        )));
        assert!(first.changes.iter().any(|change| matches!(
            change,
            PlannedChange::Add { reference, position, .. }
                if reference == "R3" && position.x > board.bounds.max_x
        )));
    }

    /// A footprint whose fields alone differ is updated, carrying only the
    /// changed fields; a field the symbol lacks is no reason to update (#788).
    #[test]
    fn a_field_only_difference_plans_an_update_carrying_the_changed_fields() {
        let mut component = resistor("R1", "/sheet/r1");
        component.fields = vec![
            symbol_field("LCSC", "C788002"),
            symbol_field("MPN", ""),
            symbol_field("Datasheet", ""),
            symbol_field("Description", "Resistor"),
        ];
        let mut footprint = board_resistor("R1", Some("/sheet/r1"));
        footprint.fields = BTreeMap::from([
            ("Datasheet".to_string(), String::new()),
            ("Description".to_string(), "Resistor".to_string()),
            ("LCSC".to_string(), "C788001".to_string()),
            ("Board only".to_string(), "kept".to_string()),
        ]);
        let design = ExportedDesign {
            root_sheet_uuid: None,
            components: vec![component],
            skipped: Vec::new(),
            unassigned: Vec::new(),
            net_names: BTreeSet::new(),
        };

        let plan = plan_sync("fields", &design, &board_with(vec![footprint.clone()]));

        assert_eq!(plan.status, PlanStatus::Ready);
        assert_eq!(plan.counts.updated.planned, 1);
        assert_eq!(plan.counts.pads_reassigned.planned, 0);
        assert!(
            matches!(plan.changes.as_slice(), [PlannedChange::Update { .. }]),
            "{:?}",
            plan.changes
        );
        let reported = serde_json::to_value(&plan.changes[0]).unwrap();
        assert_eq!(
            reported["fields"],
            serde_json::json!([
                {"name": "LCSC", "value": "C788002"},
                {"name": "MPN", "value": ""}
            ])
        );

        // Once the footprint carries the symbol's values, nothing is planned,
        // whatever else the footprint carries.
        footprint
            .fields
            .insert("LCSC".to_string(), "C788002".to_string());
        footprint.fields.insert("MPN".to_string(), String::new());
        let plan = plan_sync("fields", &design, &board_with(vec![footprint]));
        assert_eq!(plan.status, PlanStatus::Noop, "{:?}", plan.changes);
    }

    /// Build a footprint carrying one pad and one child of every graphic kind,
    /// the way a real library footprint arrives from KiCad. The existing sync
    /// test passes `&[]` for graphics, which is precisely why #244 survived it.
    fn footprint_with_artwork(reference: &str) -> prost_types::Any {
        use copperpour_ipc::gen::kiapi;
        use prost::Message;
        let silk = || "F.SilkS".to_string();
        let item = copperpour_ipc::KiCadIpcClient::build_footprint_item(
            "Package_SO:SOIC-8_3.9x4.9mm_P1.27mm",
            reference,
            "NE555",
            &[copperpour_ipc::IpcPadDefinition {
                number: "1".to_string(),
                pad_type: "smd".to_string(),
                shape: "rect".to_string(),
                x: 0.0,
                y: 0.0,
                rotation: 0.0,
                size_x: 1.0,
                size_y: 1.0,
                drill_x: None,
                drill_y: None,
                drill_oval: false,
                layers: vec!["F.Cu".to_string()],
                roundrect_ratio: 0.0,
                custom: None,
            }],
            &[
                copperpour_ipc::IpcGraphicDefinition::Line {
                    start: (-2.0, -2.5),
                    end: (2.0, -2.5),
                    layer: silk(),
                    width: 0.12,
                    line_style: copperpour_ipc::IpcLineStyle::Solid,
                },
                copperpour_ipc::IpcGraphicDefinition::Rect {
                    start: (-2.6, -3.0),
                    end: (2.6, 3.0),
                    layer: "F.CrtYd".to_string(),
                    width: 0.05,
                    line_style: copperpour_ipc::IpcLineStyle::Solid,
                    filled: false,
                },
                copperpour_ipc::IpcGraphicDefinition::Circle {
                    center: (-1.8, -1.8),
                    end: (-1.6, -1.8),
                    layer: silk(),
                    width: 0.12,
                    line_style: copperpour_ipc::IpcLineStyle::Solid,
                    filled: true,
                },
                copperpour_ipc::IpcGraphicDefinition::Arc {
                    start: (-1.0, -2.5),
                    mid: (0.0, -2.0),
                    end: (1.0, -2.5),
                    layer: "F.Fab".to_string(),
                    width: 0.1,
                    line_style: copperpour_ipc::IpcLineStyle::Solid,
                },
                copperpour_ipc::IpcGraphicDefinition::Poly {
                    points: vec![(-1.0, 2.0), (1.0, 2.0), (0.0, 2.8)],
                    layer: "F.Fab".to_string(),
                    width: 0.1,
                    line_style: copperpour_ipc::IpcLineStyle::Solid,
                    filled: true,
                },
                copperpour_ipc::IpcGraphicDefinition::Text {
                    text: "U1".to_string(),
                    position: (0.0, -3.5),
                    rotation: 0.0,
                    layer: silk(),
                    size: 1.0,
                    stroke_width_mm: 0.15,
                    keep_upright: true,
                },
            ],
            &copperpour_ipc::IpcFieldPlacement::default(),
            25.0,
            30.0,
            0.0,
            "F.Cu",
        )
        .unwrap();
        // Give it the KIID the update path matches against.
        let mut footprint =
            kiapi::board::types::FootprintInstance::decode(item.value.as_slice()).unwrap();
        footprint.id = Some(kiapi::common::types::Kiid {
            value: format!("{}-kiid", reference.to_lowercase()),
        });
        copperpour_ipc::builders::pack_any(&footprint, "kiapi.board.types.FootprintInstance")
    }

    /// Tally a footprint definition's children by the protobuf type they
    /// declare — the property #244 destroyed.
    fn child_types(item: &prost_types::Any) -> BTreeMap<String, usize> {
        use copperpour_ipc::gen::kiapi;
        use prost::Message;
        let footprint =
            kiapi::board::types::FootprintInstance::decode(item.value.as_slice()).unwrap();
        let mut counts = BTreeMap::new();
        for child in &footprint.definition.as_ref().unwrap().items {
            *counts
                .entry(copperpour_ipc::builders::any_type_name(child).to_string())
                .or_insert(0) += 1;
        }
        counts
    }

    /// #244. A footprint's pads, graphics and text all live in one repeated
    /// `Any` field, and proto3 skips field numbers it does not recognise rather
    /// than failing — so a `BoardGraphicShape` decodes cleanly as a near-empty
    /// `Pad`. Filtering that list with `Pad::decode(..).ok()` therefore matched
    /// every graphic, and packing the decoded value back re-typed it. In
    /// neusse's benchmark an 8-pad SOIC-8 came out of a sync with 28 pads —
    /// the 20 extras nameless, at (0,0), one per lost graphic — and no artwork.
    #[test]
    fn syncing_a_footprint_leaves_its_graphics_as_graphics() {
        use copperpour_ipc::gen::kiapi;
        use prost::Message;
        let item = footprint_with_artwork("U1");
        let before = child_types(&item);

        // Sanity: the fixture must actually carry the mixture, or this test
        // proves nothing — which is the trap the pre-existing sync test fell
        // into by passing `&[]` graphics.
        assert_eq!(before.get("kiapi.board.types.Pad"), Some(&1));
        assert_eq!(before.get("kiapi.board.types.BoardGraphicShape"), Some(&5));
        assert_eq!(before.get("kiapi.board.types.BoardText"), Some(&1));

        let change = PlannedChange::Update {
            kiid: "u1-kiid".to_string(),
            reference: "U1".to_string(),
            value: "NE555".to_string(),
            symbol_path: "/root/u1".to_string(),
            dnp: false,
            exclude_from_bom: false,
            pad_nets: BTreeMap::from([("1".to_string(), "GND".to_string())]),
            unconnected_pins: BTreeSet::new(),
            preserve: PreservedBoardState {
                position: Point { x: 25.0, y: 30.0 },
                rotation: 0.0,
                layer: "F.Cu".to_string(),
                locked: false,
            },
            fields: Vec::new(),
            sheet: SymbolSheet::default(),
            sheet_only: false,
            replace_footprint: None,
        };
        let updated =
            update_footprint_item(&item, &change, &BTreeMap::from([("GND".to_string(), 1)]))
                .unwrap();

        assert_eq!(
            child_types(&updated),
            before,
            "sync re-typed footprint children; graphics must survive as graphics"
        );

        // And the pad still got the net it was there to get.
        let footprint =
            kiapi::board::types::FootprintInstance::decode(updated.value.as_slice()).unwrap();
        let pad = footprint
            .definition
            .as_ref()
            .unwrap()
            .items
            .iter()
            .filter(|child| copperpour_ipc::builders::any_is(child, "kiapi.board.types.Pad"))
            .map(|child| kiapi::board::types::Pad::decode(child.value.as_slice()).unwrap())
            .next()
            .expect("the pad survived");
        assert_eq!(pad.net.as_ref().unwrap().name, "GND");
    }

    /// KiCad's Update PCB keeps a part excluded from the BOM on the board and
    /// marks its footprint; only Exclude from board keeps it off (#875).
    #[test]
    fn a_part_excluded_from_the_bom_stays_on_the_board() {
        let directory = tempfile::tempdir().unwrap();
        let schematic = directory.path().join("flags.kicad_sch");
        let symbol = |reference: &str, uuid: &str, in_bom: &str, on_board: &str| {
            format!(
                "(symbol (lib_id \"x:y\") (in_bom {in_bom}) (on_board {on_board}) (dnp no) \
                 (uuid \"{uuid}\") (instances (project \"p\" (path \"/root\" \
                 (reference \"{reference}\") (unit 1)))))"
            )
        };
        std::fs::write(
            &schematic,
            format!(
                "(kicad_sch (uuid \"root\") {} {} {})",
                symbol("TP1", "tp1", "no", "yes"),
                symbol("R1", "r1", "yes", "yes"),
                symbol("H1", "h1", "no", "no"),
            ),
        )
        .unwrap();
        let mut design = ExportedDesign {
            root_sheet_uuid: None,
            components: vec![
                resistor("TP1", "/root/tp1"),
                resistor("R1", "/root/r1"),
                resistor("H1", "/root/h1"),
            ],
            skipped: Vec::new(),
            unassigned: Vec::new(),
            net_names: BTreeSet::new(),
        };

        apply_saved_symbol_flags(&[schematic], &mut design).unwrap();

        let kept = design
            .components
            .iter()
            .map(|component| (component.reference.as_str(), component.exclude_from_bom))
            .collect::<Vec<_>>();
        assert_eq!(kept, [("TP1", true), ("R1", false)]);
        let skipped = design
            .skipped
            .iter()
            .map(|component| component.reference.as_str())
            .collect::<Vec<_>>();
        assert_eq!(skipped, ["H1"]);
    }

    /// The flags `edit_schematic_component` sets reach the plan through the
    /// sheet this module reads them from (#34): U1 excluded from the board is
    /// skipped by flag instead of added, and R1 excluded from the BOM is added
    /// with its footprint excluded from the BOM. The sheet is KiCad-saved.
    #[tokio::test]
    async fn edited_board_and_bom_flags_change_the_plan() {
        let directory = tempfile::tempdir().unwrap();
        let schematic = directory.path().join("flags.kicad_sch");
        std::fs::write(
            &schematic,
            include_str!("../../tests/fixtures/dnp_kicad10.kicad_sch"),
        )
        .unwrap();
        let plan = || {
            let mut design = ExportedDesign {
                root_sheet_uuid: None,
                components: vec![resistor("U1", "/u1"), resistor("R1", "/r1")],
                skipped: Vec::new(),
                unassigned: Vec::new(),
                net_names: BTreeSet::new(),
            };
            apply_saved_symbol_flags(std::slice::from_ref(&schematic), &mut design).unwrap();
            let plan = plan_sync("netlist", &design, &board_with(Vec::new()));
            (design, plan)
        };

        let (design, before) = plan();
        assert_eq!(before.counts.added.planned, 2);
        assert_eq!(before.counts.skipped_by_flag.planned, 0);
        assert!(design.components.iter().all(|c| !c.exclude_from_bom));

        let edit = crate::tools::sch_components::tools()
            .into_iter()
            .find(|tool| tool.name == "edit_schematic_component")
            .unwrap();
        let context = std::sync::Arc::new(crate::tools::ToolContext::new(
            crate::tools::ServerConfig::default(),
            std::sync::Arc::new(crate::router::ToolRouter::new()),
        ));
        for (reference, flag) in [("U1", "on_board"), ("R1", "in_bom")] {
            let args = serde_json::json!({
                "schematic": schematic.display().to_string(),
                "reference": reference,
                flag: false
            });
            let result = (edit.handler)(&args, context.clone()).await.unwrap();
            assert!(!result.is_error, "{:?}", result.content);
        }

        let (design, after) = plan();
        let skipped = design
            .skipped
            .iter()
            .map(|component| component.reference.as_str())
            .collect::<Vec<_>>();
        assert_eq!(skipped, ["U1"]);
        assert_eq!(after.counts.skipped_by_flag.planned, 1);
        assert_eq!(after.counts.added.planned, 1);
        let r1 = design
            .components
            .iter()
            .find(|component| component.reference == "R1")
            .unwrap();
        assert!(r1.exclude_from_bom);
    }

    #[test]
    fn a_footprint_carries_its_symbol_s_exclude_from_bom() {
        use copperpour_ipc::gen::kiapi;
        use prost::Message;
        let item = footprint_with_artwork("TP1");
        let mut footprint =
            kiapi::board::types::FootprintInstance::decode(item.value.as_slice()).unwrap();

        for exclude_from_bom in [true, false] {
            apply_footprint_fields(
                &mut footprint,
                "TP1",
                "TP",
                "/root/tp1",
                false,
                exclude_from_bom,
                &BTreeMap::new(),
                &BTreeMap::new(),
            )
            .unwrap();
            let instance = footprint.attributes.as_ref().unwrap();
            let definition = footprint
                .definition
                .as_ref()
                .unwrap()
                .attributes
                .as_ref()
                .unwrap();
            assert_eq!(instance.exclude_from_bill_of_materials, exclude_from_bom);
            assert_eq!(definition.exclude_from_bill_of_materials, exclude_from_bom);
        }
    }

    /// The add path calls `apply_footprint_fields` too (`build_mutation_items`),
    /// so a brand-new footprint was corrupted before it ever reached KiCad.
    #[test]
    fn a_newly_added_footprint_keeps_its_graphics_too() {
        use copperpour_ipc::gen::kiapi;
        use prost::Message;
        let item = footprint_with_artwork("U2");
        let before = child_types(&item);
        let mut footprint =
            kiapi::board::types::FootprintInstance::decode(item.value.as_slice()).unwrap();

        apply_footprint_fields(
            &mut footprint,
            "U2",
            "NE555",
            "/root/u2",
            false,
            false,
            &BTreeMap::from([("1".to_string(), "VCC".to_string())]),
            &BTreeMap::from([("VCC".to_string(), 3)]),
        )
        .unwrap();

        let repacked =
            copperpour_ipc::builders::pack_any(&footprint, "kiapi.board.types.FootprintInstance");
        assert_eq!(child_types(&repacked), before);
    }

    /// The invariant that would have caught #244 on its own.
    ///
    /// `create_items`/`update_items` only confirm KiCad *accepted* each item,
    /// and the reported counts are copied from the plan — so the corruption
    /// travelled all the way to a success message. Here the exact damage is
    /// reproduced (every drawing re-typed as a pad, which is what the old
    /// `Pad::decode` filter did) and the shape comparison is shown to see it.
    #[test]
    fn the_post_apply_check_sees_drawings_turned_into_pads() {
        use copperpour_ipc::gen::kiapi;
        use prost::Message;

        let sent = footprint_with_artwork("U4");
        let expected = footprint_shapes(std::iter::once(&sent));
        // The pad and drawing counts are what #244 got wrong.
        assert_eq!((expected["U4"].pads, expected["U4"].drawings), (1, 6));

        // Exactly #244: decode every child as a Pad and pack it back as one.
        let mut corrupted =
            kiapi::board::types::FootprintInstance::decode(sent.value.as_slice()).unwrap();
        for child in &mut corrupted.definition.as_mut().unwrap().items {
            if let Ok(pad) = kiapi::board::types::Pad::decode(child.value.as_slice()) {
                *child = copperpour_ipc::builders::pack_any(&pad, "kiapi.board.types.Pad");
            }
        }
        let corrupted =
            copperpour_ipc::builders::pack_any(&corrupted, "kiapi.board.types.FootprintInstance");
        let actual = footprint_shapes(std::iter::once(&corrupted));

        // The reported symptom, reproduced: the five graphic shapes each become
        // a pad. The text survives — `BoardText`'s bytes genuinely fail to
        // decode as a `Pad`, while `BoardGraphicShape`'s do not — which is why
        // #239 reported footprints losing their *graphics* while their
        // reference and value text stayed put.
        assert_eq!((actual["U4"].pads, actual["U4"].drawings), (6, 1));
        assert_ne!(actual["U4"], expected["U4"]);
    }

    /// A child that declares itself a pad and will not decode is a real
    /// failure, and has to be reported as *that*.
    ///
    /// Skipping it silently does still end in an error — the "footprint has no
    /// pad N" check downstream fires, because the pad never made it into
    /// `seen_pads` — but that error sends the reader looking for a missing pad
    /// that is in fact present and unreadable. So this asserts the specific
    /// message, not merely that something failed: a neuter that restored the
    /// silent skip passed an assertion that only checked for the reference.
    #[test]
    fn an_undecodable_pad_is_reported_not_skipped() {
        use copperpour_ipc::gen::kiapi;
        use prost::Message;
        let item = footprint_with_artwork("U3");
        let mut footprint =
            kiapi::board::types::FootprintInstance::decode(item.value.as_slice()).unwrap();
        for child in &mut footprint.definition.as_mut().unwrap().items {
            if copperpour_ipc::builders::any_is(child, "kiapi.board.types.Pad") {
                // Wire type 7 does not exist; nothing can decode this.
                child.value = vec![0xff, 0xff, 0xff];
            }
        }

        let error = apply_footprint_fields(
            &mut footprint,
            "U3",
            "NE555",
            "/root/u3",
            false,
            false,
            &BTreeMap::from([("1".to_string(), "VCC".to_string())]),
            &BTreeMap::new(),
        )
        .expect_err("an unreadable pad must not pass silently");
        let text = format!("{error:#}");
        assert!(
            text.contains("U3") && text.contains("cannot read"),
            "must say the pad is unreadable, not that it is missing: {text}"
        );
    }

    #[test]
    fn update_item_changes_only_schematic_owned_fields() {
        use copperpour_ipc::gen::kiapi;
        use prost::Message;

        let item = copperpour_ipc::KiCadIpcClient::build_footprint_item(
            "Resistor_SMD:R_0603_1608Metric",
            "R1",
            "1k",
            &[copperpour_ipc::IpcPadDefinition {
                number: "1".to_string(),
                pad_type: "smd".to_string(),
                shape: "rect".to_string(),
                x: 0.0,
                y: 0.0,
                rotation: 0.0,
                size_x: 1.0,
                size_y: 1.0,
                drill_x: None,
                drill_y: None,
                drill_oval: false,
                layers: vec!["F.Cu".to_string()],
                roundrect_ratio: 0.0,
                custom: None,
            }],
            &[],
            &copperpour_ipc::IpcFieldPlacement::default(),
            25.0,
            30.0,
            90.0,
            "F.Cu",
        )
        .unwrap();
        let mut footprint =
            kiapi::board::types::FootprintInstance::decode(item.value.as_slice()).unwrap();
        footprint.id = Some(kiapi::common::types::Kiid {
            value: "keep-kiid".to_string(),
        });
        footprint.locked = kiapi::common::types::LockedState::LsLocked as i32;
        let item =
            copperpour_ipc::builders::pack_any(&footprint, "kiapi.board.types.FootprintInstance");
        let change = PlannedChange::Update {
            kiid: "keep-kiid".to_string(),
            reference: "R2".to_string(),
            value: "10k".to_string(),
            symbol_path: "/root/symbol".to_string(),
            dnp: true,
            exclude_from_bom: false,
            pad_nets: BTreeMap::from([("1".to_string(), "VCC".to_string())]),
            unconnected_pins: BTreeSet::new(),
            preserve: PreservedBoardState {
                position: Point { x: 25.0, y: 30.0 },
                rotation: 90.0,
                layer: "F.Cu".to_string(),
                locked: true,
            },
            fields: Vec::new(),
            sheet: SymbolSheet::default(),
            sheet_only: false,
            replace_footprint: None,
        };

        let updated =
            update_footprint_item(&item, &change, &BTreeMap::from([("VCC".to_string(), 7)]))
                .unwrap();
        let updated =
            kiapi::board::types::FootprintInstance::decode(updated.value.as_slice()).unwrap();

        assert_eq!(updated.id.as_ref().unwrap().value, "keep-kiid");
        assert_eq!(updated.position, footprint.position);
        assert_eq!(updated.orientation, footprint.orientation);
        assert_eq!(updated.layer, footprint.layer);
        assert_eq!(updated.locked, footprint.locked);
        assert!(updated.attributes.as_ref().unwrap().do_not_populate);
        let pad = updated
            .definition
            .as_ref()
            .unwrap()
            .items
            .iter()
            .find_map(|item| kiapi::board::types::Pad::decode(item.value.as_slice()).ok())
            .unwrap();
        assert_eq!(pad.net.as_ref().unwrap().name, "VCC");
        assert_eq!(pad.net.as_ref().unwrap().code.as_ref().unwrap().value, 7);
    }

    /// KiCad 10's own `R1`, as its IPC API sent it (`issue_474_r1.ipc.bin`):
    /// Datasheet and Description empty, hidden on F.Fab, and no custom field.
    fn captured_r1() -> copperpour_ipc::gen::kiapi::board::types::FootprintInstance {
        copperpour_ipc::gen::kiapi::board::types::FootprintInstance::decode(
            include_bytes!("../../tests/fixtures/issue_474_r1.ipc.bin").as_slice(),
        )
        .expect("the checked-in KiCad IPC capture must decode")
    }

    /// A custom field as someone might have left it: shown on the silkscreen
    /// beside the part, in 2 mm text, turned. Built by hand, so the style the
    /// sync gives an added field cannot leak into it.
    fn shown_custom_field(
        name: &str,
        value: &str,
        footprint: &copperpour_ipc::gen::kiapi::board::types::FootprintInstance,
    ) -> copperpour_ipc::gen::kiapi::board::types::Field {
        use copperpour_ipc::gen::kiapi::{board::types as board, common::types as common};

        let origin = footprint.position.expect("a placed footprint");
        board::Field {
            id: None,
            name: name.to_string(),
            text: Some(board::BoardText {
                id: None,
                text: Some(common::Text {
                    position: Some(common::Vector2 {
                        x_nm: origin.x_nm + 2_500_000,
                        y_nm: origin.y_nm - 1_000_000,
                    }),
                    attributes: Some(common::TextAttributes {
                        size: Some(common::Vector2 {
                            x_nm: 2_000_000,
                            y_nm: 2_000_000,
                        }),
                        stroke_width: Some(common::Distance { value_nm: 300_000 }),
                        angle: Some(common::Angle {
                            value_degrees: 90.0,
                        }),
                        line_spacing: 1.0,
                        keep_upright: true,
                        visible: true,
                        ..Default::default()
                    }),
                    text: value.to_string(),
                    hyperlink: String::new(),
                }),
                layer: board::BoardLayer::BlFSilkS as i32,
                knockout: false,
                locked: common::LockedState::LsLocked as i32,
                parent: None,
            }),
            visible: true,
        }
    }

    fn with_field_item(
        mut footprint: copperpour_ipc::gen::kiapi::board::types::FootprintInstance,
        field: &copperpour_ipc::gen::kiapi::board::types::Field,
    ) -> copperpour_ipc::gen::kiapi::board::types::FootprintInstance {
        footprint
            .definition
            .as_mut()
            .unwrap()
            .items
            .push(copperpour_ipc::builders::pack_any(
                field,
                "kiapi.board.types.Field",
            ));
        footprint
    }

    /// The `Field` items of a footprint's definition, by name.
    fn field_items(
        footprint: &copperpour_ipc::gen::kiapi::board::types::FootprintInstance,
    ) -> BTreeMap<String, copperpour_ipc::gen::kiapi::board::types::Field> {
        footprint
            .definition
            .as_ref()
            .unwrap()
            .items
            .iter()
            .filter(|child| copperpour_ipc::builders::any_is(child, "kiapi.board.types.Field"))
            .map(|child| {
                let field =
                    copperpour_ipc::gen::kiapi::board::types::Field::decode(child.value.as_slice())
                        .unwrap();
                (field.name.clone(), field)
            })
            .collect()
    }

    /// What a field shows, everything but its text.
    fn presentation(
        field: &copperpour_ipc::gen::kiapi::board::types::Field,
    ) -> (
        i32,
        bool,
        i32,
        Option<copperpour_ipc::gen::kiapi::common::types::Vector2>,
        Option<copperpour_ipc::gen::kiapi::common::types::TextAttributes>,
    ) {
        let board_text = field.text.as_ref().unwrap();
        let text = board_text.text.as_ref().unwrap();
        (
            board_text.layer,
            field.visible,
            board_text.locked,
            text.position,
            text.attributes.clone(),
        )
    }

    /// `live` after a planned update that writes `fields`.
    fn updated_with(
        live: &copperpour_ipc::gen::kiapi::board::types::FootprintInstance,
        fields: Vec<SymbolField>,
    ) -> copperpour_ipc::gen::kiapi::board::types::FootprintInstance {
        let board = board_footprint_from_instance(live).unwrap();
        let change = PlannedChange::Update {
            kiid: board.kiid.clone(),
            reference: board.reference.clone(),
            value: board.value.clone(),
            symbol_path: "/root/r1".to_string(),
            dnp: false,
            exclude_from_bom: false,
            pad_nets: board.pad_nets.clone(),
            unconnected_pins: BTreeSet::new(),
            preserve: PreservedBoardState {
                position: board.position,
                rotation: board.rotation,
                layer: board.layer.clone(),
                locked: board.locked,
            },
            fields,
            sheet: board.sheet.clone(),
            sheet_only: false,
            replace_footprint: None,
        };
        let item = copperpour_ipc::builders::pack_any(live, "kiapi.board.types.FootprintInstance");
        let updated = update_footprint_item(&item, &change, &BTreeMap::new()).unwrap();
        copperpour_ipc::gen::kiapi::board::types::FootprintInstance::decode(
            updated.value.as_slice(),
        )
        .unwrap()
    }

    /// The snapshot reads what the planner compares: Datasheet and Description
    /// from their own slots, and each custom field by name (#788).
    #[test]
    fn the_snapshot_reads_a_footprints_fields_by_name() {
        let r1 = captured_r1();
        let shown = shown_custom_field("LCSC", "C1", &r1);

        let board = board_footprint_from_instance(&with_field_item(r1, &shown)).unwrap();

        assert_eq!(
            board.fields,
            BTreeMap::from([
                ("Datasheet".to_string(), String::new()),
                ("Description".to_string(), String::new()),
                ("LCSC".to_string(), "C1".to_string()),
            ])
        );
    }

    /// A field the snapshot cannot read refuses it, naming the footprint. Read
    /// as absent, it would plan a second field of the same name.
    #[test]
    fn an_undecodable_field_refuses_the_snapshot() {
        let r1 = captured_r1();
        let shown = shown_custom_field("LCSC", "C1", &r1);
        let mut footprint = with_field_item(r1, &shown);
        for child in &mut footprint.definition.as_mut().unwrap().items {
            if copperpour_ipc::builders::any_is(child, "kiapi.board.types.Field") {
                // Wire type 7 does not exist; nothing can decode this.
                child.value = vec![0xff, 0xff, 0xff];
            }
        }

        let error = board_footprint_from_instance(&footprint)
            .expect_err("an unreadable field must not pass silently");
        let text = format!("{error:#}");
        assert!(
            text.contains("R1") && text.contains("cannot read"),
            "must name the footprint and say the field is unreadable: {text}"
        );
    }

    /// On an update, a field the footprint has takes the symbol's text and
    /// keeps everything else: layer, visibility, lock, position and font. Both
    /// KiCad's own Description and a custom field on the silkscreen (#788).
    #[test]
    fn an_update_changes_only_the_text_of_a_field_the_footprint_has() {
        let r1 = captured_r1();
        let shown = shown_custom_field("LCSC", "C1", &r1);
        let live = with_field_item(r1, &shown);

        let updated = updated_with(
            &live,
            vec![
                symbol_field("LCSC", "C788002"),
                symbol_field("Description", "Resistor"),
            ],
        );

        assert_eq!(field_text(&updated.description_field), "Resistor");
        assert_eq!(
            presentation(updated.description_field.as_ref().unwrap()),
            presentation(live.description_field.as_ref().unwrap())
        );
        assert_eq!(
            updated.datasheet_field, live.datasheet_field,
            "a field the update does not name is as KiCad sent it"
        );
        let custom = field_items(&updated);
        assert_eq!(custom.len(), 1, "{:?}", custom.keys());
        assert_eq!(field_text(&Some(custom["LCSC"].clone())), "C788002");
        assert_eq!(presentation(&custom["LCSC"]), presentation(&shown));
    }

    /// A field the footprint lacks is added as Update PCB adds it: hidden on
    /// F.Fab at the footprint's position in default fabrication text, and on
    /// the back on B.Fab, mirrored, at the footprint's normalised angle (#788).
    #[test]
    fn an_update_adds_a_field_the_footprint_lacks_as_update_pcb_adds_it() {
        use copperpour_ipc::gen::kiapi::{board::types::BoardLayer, common::types as common};

        let front = captured_r1();
        let added = field_items(&updated_with(
            &front,
            vec![symbol_field("MPN", "MPN-788-R1")],
        ))
        .remove("MPN")
        .expect("an added field");
        assert_eq!(field_text(&Some(added.clone())), "MPN-788-R1");
        let (layer, visible, locked, position, attributes) = presentation(&added);
        let attributes = attributes.unwrap();
        assert_eq!(layer, BoardLayer::BlFFab as i32);
        assert!(!visible);
        assert_eq!(locked, common::LockedState::LsUnlocked as i32);
        assert_eq!(position, front.position);
        assert_eq!(
            attributes.size,
            Some(common::Vector2 {
                x_nm: 1_000_000,
                y_nm: 1_000_000
            })
        );
        assert_eq!(attributes.stroke_width.unwrap().value_nm, 150_000);
        assert_eq!(attributes.angle.unwrap().value_degrees, 0.0);
        assert!(!attributes.mirrored && !attributes.keep_upright && !attributes.italic);
        assert!(attributes.multiline);

        let mut back = captured_r1();
        back.layer = BoardLayer::BlBCu as i32;
        back.orientation = Some(common::Angle {
            value_degrees: -90.0,
        });
        let added = field_items(&updated_with(
            &back,
            vec![symbol_field("MPN", "MPN-788-R1")],
        ))
        .remove("MPN")
        .expect("an added field");
        let (layer, _, _, _, attributes) = presentation(&added);
        let attributes = attributes.unwrap();
        assert_eq!(layer, BoardLayer::BlBFab as i32);
        assert!(attributes.mirrored);
        assert_eq!(attributes.angle.unwrap().value_degrees, 270.0);
    }

    /// A missing Datasheet is made as KiCad's footprint constructor makes it,
    /// checked against KiCad's own captured empty Datasheet (#788).
    #[test]
    fn a_missing_datasheet_is_made_as_kicads_footprint_constructor_makes_it() {
        let kicad = captured_r1();
        let mut built = captured_r1();
        built.datasheet_field = None;

        apply_symbol_fields(&mut built, &[symbol_field("Datasheet", "r1-datasheet")]).unwrap();

        assert_eq!(field_text(&built.datasheet_field), "r1-datasheet");
        assert_eq!(
            presentation(built.datasheet_field.as_ref().unwrap()),
            presentation(kicad.datasheet_field.as_ref().unwrap())
        );
    }

    #[test]
    fn removing_a_pad_from_a_routed_net_conflicts_the_whole_plan() {
        let design = ExportedDesign {
            root_sheet_uuid: None,
            components: vec![DesignComponent {
                pad_nets: BTreeMap::new(),
                ..resistor("R1", "/sheet/existing")
            }],
            skipped: Vec::new(),
            unassigned: Vec::new(),
            net_names: BTreeSet::new(),
        };
        let board = BoardState {
            footprints: vec![BoardFootprint {
                kiid: "existing-kiid".to_string(),
                reference: "R1".to_string(),
                value: "10k".to_string(),
                footprint_id: "Resistor_SMD:R_0603_1608Metric".to_string(),
                symbol_path: Some("/sheet/existing".to_string()),
                pad_nets: BTreeMap::from([("1".to_string(), "VCC".to_string())]),
                pad_numbers: BTreeSet::from(["1".to_string(), "2".to_string()]),
                position: Point { x: 1.0, y: 2.0 },
                rotation: 0.0,
                layer: "F.Cu".to_string(),
                locked: false,
                dnp: false,
                exclude_from_bom: false,
                not_in_schematic: false,
                fields: BTreeMap::new(),
                sheet: SymbolSheet::default(),
            }],
            routed_nets: BTreeMap::from([("VCC".to_string(), 1)]),
            bounds: Bounds {
                min_x: 0.0,
                min_y: 0.0,
                max_x: 10.0,
                max_y: 10.0,
            },
        };

        let plan = plan_sync("netlist", &design, &board);

        assert_eq!(plan.status, PlanStatus::Conflict);
        assert!(plan.changes.is_empty());
        assert!(plan
            .diagnostics
            .iter()
            .any(|diagnostic| diagnostic.code == "routed_pad_net_change"));
    }

    /// `R1` with pad 1 on `VCC` and pad 2 on `GND`, both nets routed, and a
    /// schematic that moves pad 1 to `VCC_A` and pad 2 to `GND_A`: two
    /// `routed_pad_net_change` conflicts (#38).
    fn routed_r1_moving_both_pads() -> (ExportedDesign, BoardState) {
        let design = ExportedDesign {
            root_sheet_uuid: None,
            components: vec![DesignComponent {
                pad_nets: BTreeMap::from([
                    ("1".to_string(), "VCC_A".to_string()),
                    ("2".to_string(), "GND_A".to_string()),
                ]),
                ..resistor("R1", "/sheet/existing")
            }],
            skipped: Vec::new(),
            unassigned: Vec::new(),
            net_names: BTreeSet::new(),
        };
        let mut board = board_with(vec![board_resistor("R1", Some("/sheet/existing"))]);
        board.routed_nets = BTreeMap::from([("VCC".to_string(), 1), ("GND".to_string(), 3)]);
        (design, board)
    }

    fn acknowledging(reference: &str, pad: &str, from: &str, to: &str) -> AcknowledgedNetChange {
        AcknowledgedNetChange {
            reference: reference.to_string(),
            pad: pad.to_string(),
            from: from.to_string(),
            to: to.to_string(),
        }
    }

    /// The codes of a plan's diagnostics that refuse it.
    fn refusing_codes(plan: &SyncPlan) -> Vec<&str> {
        plan.diagnostics
            .iter()
            .filter(|diagnostic| diagnostic.code != "routed_pad_net_change_acknowledged")
            .map(|diagnostic| diagnostic.code.as_str())
            .collect()
    }

    /// #38: each routed pad net change named exactly, as its diagnostic
    /// reports it, is planned as an ordinary pad change and noted.
    #[test]
    fn acknowledged_routed_pad_net_changes_are_planned_and_noted() {
        let (design, board) = routed_r1_moving_both_pads();
        let refused = plan_sync("netlist", &design, &board);
        assert_eq!(refused.status, PlanStatus::Conflict);
        let reported = refused
            .diagnostics
            .iter()
            .filter(|diagnostic| diagnostic.code == "routed_pad_net_change")
            .map(|diagnostic| {
                (
                    diagnostic.reference.clone().unwrap(),
                    diagnostic.net_change.clone().unwrap(),
                )
            })
            .collect::<Vec<_>>();
        let change = |pad: &str, from: &str, to: &str| NetChange {
            pad: pad.to_string(),
            from: from.to_string(),
            to: to.to_string(),
        };
        assert_eq!(
            reported,
            vec![
                ("R1".to_string(), change("1", "VCC", "VCC_A")),
                ("R1".to_string(), change("2", "GND", "GND_A")),
            ]
        );
        let acknowledged = reported
            .iter()
            .map(|(reference, change)| {
                acknowledging(reference, &change.pad, &change.from, &change.to)
            })
            .collect();

        let plan = plan_sync_acknowledging("netlist", &design, &board, &acknowledged, false);

        assert_eq!(plan.status, PlanStatus::Ready, "{:?}", plan.diagnostics);
        assert_eq!(refusing_codes(&plan), Vec::<&str>::new());
        assert_eq!(plan.counts.updated.planned, 1);
        assert_eq!(plan.counts.pads_reassigned.planned, 2);
        let PlannedChange::Update { pad_nets, .. } = &plan.changes[0] else {
            panic!("the acknowledged change is an update");
        };
        assert_eq!(pad_nets, &design.components[0].pad_nets);
        let noted = plan
            .diagnostics
            .iter()
            .map(|diagnostic| {
                (
                    diagnostic.code.as_str(),
                    diagnostic.net_change.clone().unwrap(),
                )
            })
            .collect::<Vec<_>>();
        assert_eq!(
            noted,
            vec![
                (
                    "routed_pad_net_change_acknowledged",
                    change("1", "VCC", "VCC_A")
                ),
                (
                    "routed_pad_net_change_acknowledged",
                    change("2", "GND", "GND_A")
                ),
            ]
        );
    }

    /// #38: an acknowledgement lets through only the change it names. The
    /// other routed change and any other conflict still refuse the plan, and
    /// an acknowledgement of a change the plan does not make refuses it too.
    #[test]
    fn an_acknowledgement_lets_through_only_the_change_it_names() {
        let (design, board) = routed_r1_moving_both_pads();
        let pad_1 = acknowledging("R1", "1", "VCC", "VCC_A");
        let pad_2 = acknowledging("R1", "2", "GND", "GND_A");
        let unmatched = "net_change_acknowledgement_unmatched";
        for (acknowledged, codes) in [
            (vec![pad_1.clone()], vec!["routed_pad_net_change"]),
            // Another target net, another pad, another part.
            (
                vec![pad_1.clone(), acknowledging("R1", "2", "GND", "GND_B")],
                vec!["routed_pad_net_change", unmatched],
            ),
            (
                vec![
                    pad_1.clone(),
                    pad_2.clone(),
                    acknowledging("R1", "3", "GND", "GND_A"),
                ],
                vec![unmatched],
            ),
            (
                vec![
                    pad_1.clone(),
                    pad_2.clone(),
                    acknowledging("R2", "1", "VCC", "VCC_A"),
                ],
                vec![unmatched],
            ),
        ] {
            let plan = plan_sync_acknowledging(
                "netlist",
                &design,
                &board,
                &acknowledged.iter().cloned().collect(),
                false,
            );
            assert_eq!(plan.status, PlanStatus::Conflict, "{acknowledged:?}");
            assert!(plan.changes.is_empty(), "{acknowledged:?}");
            assert_eq!(refusing_codes(&plan), codes, "{acknowledged:?}");
        }

        // With no routed copper the changes need no acknowledgement, so one
        // given for them matches nothing.
        let unrouted = board_with(board.footprints.clone());
        let plan = plan_sync_acknowledging(
            "netlist",
            &design,
            &unrouted,
            &BTreeSet::from([pad_1.clone(), pad_2.clone()]),
            false,
        );
        assert_eq!(plan.status, PlanStatus::Conflict);
        assert_eq!(refusing_codes(&plan), vec![unmatched, unmatched]);

        // A conflict of another kind is not waived.
        let mut duplicated = board.clone();
        duplicated.footprints.push(board_resistor("R1", None));
        let plan = plan_sync_acknowledging(
            "netlist",
            &design,
            &duplicated,
            &BTreeSet::from([pad_1, pad_2]),
            false,
        );
        assert_eq!(plan.status, PlanStatus::Conflict);
        assert_eq!(refusing_codes(&plan), vec!["duplicate_board_reference"]);
    }

    #[test]
    fn already_synchronized_design_is_noop() {
        let design = ExportedDesign {
            root_sheet_uuid: None,
            components: vec![resistor("R1", "/sheet/existing")],
            skipped: Vec::new(),
            unassigned: Vec::new(),
            net_names: BTreeSet::new(),
        };
        let plan = plan_sync(
            "netlist",
            &design,
            &board_with(vec![board_resistor("R1", Some("/sheet/existing"))]),
        );

        assert_eq!(plan.status, PlanStatus::Noop);
        assert!(plan.changes.is_empty());
        assert_eq!(plan.counts.conflicts.planned, 0);
    }

    /// A footprint the schematic names differently is replaced, as KiCad's
    /// Update PCB replaces it; a locked one keeps its footprint, with a note
    /// that refuses nothing, as KiCad warns. The new footprint's pads are
    /// checked in the handler's preflight, not here.
    #[test]
    fn a_changed_footprint_is_replaced_unless_locked_and_an_unrouted_net_change_is_planned() {
        let design = ExportedDesign {
            root_sheet_uuid: None,
            components: vec![resistor("R1", "/sheet/existing")],
            skipped: Vec::new(),
            unassigned: Vec::new(),
            net_names: BTreeSet::new(),
        };
        let mut footprint = board_resistor("R1", Some("/sheet/existing"));
        footprint.footprint_id = "Resistor_SMD:R_0805_2012Metric".to_string();
        let to = design.components[0].footprint_id.clone();
        let swap = plan_sync("netlist", &design, &board_with(vec![footprint.clone()]));
        assert_eq!(swap.status, PlanStatus::Ready, "{:?}", swap.diagnostics);
        assert_eq!(swap.counts.updated.planned, 1);
        let PlannedChange::Update {
            replace_footprint,
            fields,
            ..
        } = &swap.changes[0]
        else {
            panic!("a replacement is an update");
        };
        assert_eq!(
            replace_footprint,
            &Some(ReplacedFootprint {
                from: "Resistor_SMD:R_0805_2012Metric".to_string(),
                to: to.clone(),
            })
        );
        // The new footprint's fields replace the old ones, so the symbol's
        // are all set again.
        assert_eq!(fields, &design.components[0].fields);
        assert_eq!(
            serde_json::to_value(&swap.changes[0]).unwrap()["replace_footprint"]["to"],
            to.as_str()
        );

        footprint.locked = true;
        let locked = plan_sync("netlist", &design, &board_with(vec![footprint]));
        assert_eq!(locked.status, PlanStatus::Noop, "{:?}", locked.diagnostics);
        assert_eq!(locked.counts.conflicts.planned, 0);
        assert_eq!(locked.diagnostics.len(), 1);
        assert_eq!(locked.diagnostics[0].code, "footprint_locked_not_replaced");

        let mut footprint = board_resistor("R1", Some("/sheet/existing"));
        footprint
            .pad_nets
            .insert("1".to_string(), "OLD_VCC".to_string());
        let net_change = plan_sync("netlist", &design, &board_with(vec![footprint]));
        assert_eq!(net_change.status, PlanStatus::Ready);
        assert_eq!(net_change.counts.pads_reassigned.planned, 1);
    }

    /// R1 marked Exclude from board, with `symbol_path` as its identity.
    fn r1_excluded() -> ExportedDesign {
        ExportedDesign {
            root_sheet_uuid: None,
            components: Vec::new(),
            skipped: vec![SkippedComponent {
                reference: "R1".to_string(),
                symbol_path: "/sheet/existing".to_string(),
            }],
            unassigned: Vec::new(),
            net_names: BTreeSet::new(),
        }
    }

    fn r1_without_a_symbol(reason: WithoutSymbolReason, locked: bool) -> FootprintWithoutSymbol {
        FootprintWithoutSymbol {
            reference: "R1".to_string(),
            kiid: "R1-kiid".to_string(),
            footprint_id: "Resistor_SMD:R_0603_1608Metric".to_string(),
            locked,
            reason,
        }
    }

    /// KiCad's Update PCB skips a symbol marked Exclude from board and treats
    /// its footprint as one with no symbol (#870): the footprint is kept and
    /// named, and nothing else is refused.
    #[test]
    fn on_board_no_skips_absent_and_names_its_footprint_when_present() {
        let design = r1_excluded();
        let absent = plan_sync("netlist", &design, &board_with(Vec::new()));
        assert_eq!(absent.status, PlanStatus::Noop);
        assert_eq!(absent.counts.skipped_by_flag.planned, 1);
        assert!(absent.without_symbols.is_empty());

        let present = plan_sync(
            "netlist",
            &design,
            &board_with(vec![board_resistor("R1", Some("/sheet/existing"))]),
        );
        assert_eq!(
            present.status,
            PlanStatus::Noop,
            "{:?}",
            present.diagnostics
        );
        assert!(present.diagnostics.is_empty(), "{:?}", present.diagnostics);
        assert_eq!(present.counts.skipped_by_flag.planned, 1);
        assert_eq!(present.counts.board_only_preserved.planned, 1);
        assert_eq!(
            present.without_symbols,
            vec![r1_without_a_symbol(
                WithoutSymbolReason::ExcludedFromBoard,
                false
            )]
        );
    }

    /// The rest of the schematic syncs beside it: R2 is added.
    #[test]
    fn an_excluded_part_on_the_board_leaves_the_rest_to_sync() {
        let mut design = r1_excluded();
        design.components.push(resistor("R2", "/sheet/new"));

        let plan = plan_sync(
            "netlist",
            &design,
            &board_with(vec![board_resistor("R1", Some("/sheet/existing"))]),
        );

        assert_eq!(plan.status, PlanStatus::Ready, "{:?}", plan.diagnostics);
        assert_eq!(plan.counts.added.planned, 1);
        assert_eq!(plan.without_symbols.len(), 1, "{:?}", plan.without_symbols);
        assert_eq!(
            plan.without_symbols[0].reason,
            WithoutSymbolReason::ExcludedFromBoard
        );
    }

    /// With `delete_footprints_without_symbols`, it is deleted, as KiCad
    /// deletes it with "Delete footprints with no symbols"; the change says
    /// why.
    #[test]
    fn an_excluded_part_s_footprint_is_deleted_on_request() {
        let plan = plan_sync_acknowledging(
            "netlist",
            &r1_excluded(),
            &board_with(vec![board_resistor("R1", Some("/sheet/existing"))]),
            &BTreeSet::new(),
            true,
        );

        assert_eq!(plan.status, PlanStatus::Ready, "{:?}", plan.diagnostics);
        assert_eq!(
            plan.changes,
            vec![PlannedChange::Delete {
                kiid: "R1-kiid".to_string(),
                reference: "R1".to_string(),
                footprint_id: "Resistor_SMD:R_0603_1608Metric".to_string(),
                reason: WithoutSymbolReason::ExcludedFromBoard,
            }]
        );
        assert_eq!(plan.counts.deleted.planned, 1);
        assert_eq!(plan.counts.board_only_preserved.planned, 0);
        assert!(plan.without_symbols.is_empty());
    }

    /// A planned deletion keeps its reason when the plan is refused, and in
    /// the list the apply reads back against.
    #[test]
    fn an_excluded_part_s_deletion_keeps_its_reason_when_refused_or_read_back() {
        let mut plan = plan_sync_acknowledging(
            "netlist",
            &r1_excluded(),
            &board_with(vec![board_resistor("R1", Some("/sheet/existing"))]),
            &BTreeSet::new(),
            true,
        );
        let excluded = vec![r1_without_a_symbol(
            WithoutSymbolReason::ExcludedFromBoard,
            false,
        )];
        assert_eq!(planned_deletions(&plan), excluded);

        refuse_plan(
            &mut plan,
            vec![conflict("preflight_conflict", "refused".to_string(), None)],
        );

        assert!(plan.changes.is_empty(), "{:?}", plan.changes);
        assert_eq!(plan.without_symbols, excluded);
    }

    /// Locked, it is kept with a note that names the reason; with the
    /// board-only attribute, it is neither deleted nor named.
    #[test]
    fn a_locked_or_board_only_excluded_footprint_is_kept() {
        let mut footprint = board_resistor("R1", Some("/sheet/existing"));
        footprint.locked = true;
        let locked = plan_sync_acknowledging(
            "netlist",
            &r1_excluded(),
            &board_with(vec![footprint]),
            &BTreeSet::new(),
            true,
        );
        assert_eq!(locked.status, PlanStatus::Noop);
        assert_eq!(
            locked.without_symbols,
            vec![r1_without_a_symbol(
                WithoutSymbolReason::ExcludedFromBoard,
                true
            )]
        );
        assert_eq!(locked.diagnostics.len(), 1, "{:?}", locked.diagnostics);
        assert_eq!(locked.diagnostics[0].code, "footprint_locked_not_deleted");
        assert!(
            locked.diagnostics[0]
                .message
                .contains("has its symbol excluded from the board"),
            "{}",
            locked.diagnostics[0].message
        );

        let mut footprint = board_resistor("R1", Some("/sheet/existing"));
        footprint.not_in_schematic = true;
        let board_only = plan_sync_acknowledging(
            "netlist",
            &r1_excluded(),
            &board_with(vec![footprint]),
            &BTreeSet::new(),
            true,
        );
        assert_eq!(board_only.status, PlanStatus::Noop);
        assert!(board_only.changes.is_empty(), "{:?}", board_only.changes);
        assert!(board_only.without_symbols.is_empty());
        assert_eq!(board_only.counts.board_only_preserved.planned, 1);
    }

    /// The excluded symbol claims a footprint as a component does: by
    /// identity, or by reference only when the footprint has no identity. A
    /// footprint with its reference but another symbol's identity lost that
    /// symbol, and is named for that reason.
    #[test]
    fn an_excluded_symbol_claims_a_footprint_by_reference_only_without_an_identity() {
        let unlinked = plan_sync(
            "netlist",
            &r1_excluded(),
            &board_with(vec![board_resistor("R1", None)]),
        );
        assert_eq!(
            unlinked.without_symbols,
            vec![r1_without_a_symbol(
                WithoutSymbolReason::ExcludedFromBoard,
                false
            )]
        );

        let another = plan_sync(
            "netlist",
            &r1_excluded(),
            &board_with(vec![board_resistor("R1", Some("/sheet/gone"))]),
        );
        assert_eq!(
            another.status,
            PlanStatus::Noop,
            "{:?}",
            another.diagnostics
        );
        assert_eq!(
            another.without_symbols,
            vec![r1_without_a_symbol(
                WithoutSymbolReason::SymbolRemoved,
                false
            )]
        );
    }

    /// The footprint an excluded symbol claims is no rename candidate for a
    /// new part, as it was not while the sync refused it.
    #[test]
    fn an_excluded_part_s_footprint_is_no_rename_candidate() {
        let mut design = r1_excluded();
        design.components.push(resistor("R2", "/sheet/new"));

        let plan = plan_sync(
            "netlist",
            &design,
            &board_with(vec![board_resistor("R1", None)]),
        );

        assert_eq!(plan.status, PlanStatus::Ready, "{:?}", plan.diagnostics);
        assert_eq!(plan.counts.added.planned, 1);
        assert_eq!(
            plan.without_symbols,
            vec![r1_without_a_symbol(
                WithoutSymbolReason::ExcludedFromBoard,
                false
            )]
        );
    }

    #[test]
    fn reference_only_possible_rename_is_a_conflict() {
        let design = ExportedDesign {
            root_sheet_uuid: None,
            components: vec![resistor("R2", "/sheet/existing")],
            skipped: Vec::new(),
            unassigned: Vec::new(),
            net_names: BTreeSet::new(),
        };
        let plan = plan_sync(
            "netlist",
            &design,
            &board_with(vec![board_resistor("R1", None)]),
        );

        assert_eq!(plan.status, PlanStatus::Conflict);
        assert!(plan
            .diagnostics
            .iter()
            .any(|diagnostic| diagnostic.code == "reference_only_rename_ambiguous"));
    }

    #[test]
    fn empty_and_duplicate_component_exports_are_rejected() {
        let empty = parse_exported_netlist("(export (components) (nets))")
            .unwrap_err()
            .to_string();
        assert!(empty.contains("zero components"), "{empty}");

        let duplicate = r#"
(export
  (components
    (comp (ref "R1") (value "1k") (footprint "Resistor_SMD:R_0603_1608Metric")
      (sheetpath (tstamps "/one/")) (tstamps "one"))
    (comp (ref "R1") (value "2k") (footprint "Resistor_SMD:R_0603_1608Metric")
      (sheetpath (tstamps "/two/")) (tstamps "two")))
  (nets))
"#;
        let duplicate = parse_exported_netlist(duplicate).unwrap_err().to_string();
        assert!(
            duplicate.contains("duplicate component reference R1"),
            "{duplicate}"
        );
    }

    #[test]
    fn plan_revision_changes_when_reviewed_board_bounds_change() {
        let design = ExportedDesign {
            root_sheet_uuid: None,
            components: vec![resistor("R1", "/sheet/new")],
            skipped: Vec::new(),
            unassigned: Vec::new(),
            net_names: BTreeSet::new(),
        };
        let first = plan_sync("netlist", &design, &board_with(Vec::new()));
        let mut changed_board = board_with(Vec::new());
        changed_board.bounds.max_x = 11.0;
        let second = plan_sync("netlist", &design, &changed_board);

        assert_ne!(first.plan_revision, second.plan_revision);
    }

    /// A plan revision must survive the clock. `kicad-cli` stamps the export
    /// time and its own version into every netlist, so hashing the raw source
    /// changed the revision every second — and apply, which requires the
    /// revision a dry run returned, could then only succeed if both calls
    /// landed inside the same wall-clock second.
    #[test]
    fn plan_revision_ignores_the_export_timestamp_and_tool_version() {
        let netlist = |date: &str, tool: &str| {
            format!(
                "(export (version \"E\")
  (design
    (source \"/tmp/x.kicad_sch\")
    (date \"{date}\")
    (tool \"{tool}\")
  )
  (components
    (comp (ref \"R1\")
      (value \"10k\")
      (footprint \"Resistor_SMD:R_0805\")
      (tstamps \"/aaa\")))
  (nets
    (net (code \"1\") (name \"GND\")
      (node (ref \"R1\") (pin \"1\")))))
"
            )
        };
        let board = BoardState {
            footprints: Vec::new(),
            routed_nets: BTreeMap::new(),
            bounds: Bounds {
                min_x: 0.0,
                min_y: 0.0,
                max_x: 0.0,
                max_y: 0.0,
            },
        };
        let a = plan_revision(
            &netlist("2026-08-15T14:48:16", "kicad-cli (10.0.5)"),
            &board,
        );
        let b = plan_revision(
            &netlist("2026-08-15T14:48:18", "kicad-cli (10.0.5)"),
            &board,
        );
        assert_eq!(a, b, "two seconds apart is not a design change");

        let c = plan_revision(
            &netlist("2026-08-15T14:48:16", "kicad-cli (10.1.0)"),
            &board,
        );
        assert_eq!(a, c, "a KiCad upgrade is not a design change");

        // A real change still moves it, or the guard is worthless.
        let changed = netlist("2026-08-15T14:48:16", "kicad-cli (10.0.5)")
            .replace("Resistor_SMD:R_0805", "Resistor_SMD:R_0603");
        assert_ne!(
            a,
            plan_revision(&changed, &board),
            "a footprint swap must move the revision"
        );
    }

    #[test]
    fn plan_revision_keeps_nested_and_quoted_design_content() {
        let netlist = |nested_date: &str, value: &str| {
            format!(
                r#"(export
  (design (date "2026-08-15T14:48:16") (tool "kicad-cli (10.0.5)"))
  (components
    (comp (ref "R1")
      (value "{value}")
      (footprint "Resistor_SMD:R_0805")
      (date "{nested_date}")
      (tstamps "/aaa")))
  (nets
    (net (code "1") (name "GND")
      (node (ref "R1") (pin "1")))))"#
            )
        };
        let board = board_with(Vec::new());
        let baseline = plan_revision(&netlist("2025-01-01", "literal (tool alpha)"), &board);

        assert_ne!(
            baseline,
            plan_revision(&netlist("2025-01-02", "literal (tool alpha)"), &board),
            "a nested date node is component content, not export metadata"
        );
        assert_ne!(
            baseline,
            plan_revision(&netlist("2025-01-01", "literal (tool beta)"), &board),
            "tool-like text inside a quoted value is design content"
        );
    }

    /// A real `FootprintInstance`, exactly as KiCad 10.0.6's IPC layer sent it
    /// for a mounting hole with no schematic symbol behind it.
    ///
    /// Captured from a running editor rather than hand-built, because a
    /// hand-built one encodes whatever the author assumed. This one already
    /// corrected two such assumptions: KiCad reports `symbol_path` as
    /// **present and empty** (not absent, and `path_human_readable` is `""`,
    /// not `"/"`), and it leaves `not_in_schematic` **false** on a board-only
    /// mounting hole — marking it `exclude_from_position_files` and
    /// `exclude_from_bill_of_materials` instead. The first hand-written
    /// fixture set that flag true, which made the rename branch below
    /// unreachable from its own tests.
    ///
    /// Regenerate with `COPPERPOUR_CAPTURE_IPC_FIXTURE=1` on the ignored live
    /// test `kicad_reports_an_empty_sheet_path_for_a_board_only_footprint`;
    /// provenance is in the fixture's README.
    const BOARD_ONLY_CAPTURE: &[u8] =
        include_bytes!("../../tests/fixtures/board_only_footprint.ipc.bin");

    /// The captured footprint, with only its identity fields varied.
    ///
    /// A board carries several board-only graphics, and the planner keys on
    /// reference and KIID, so tests need more than one. Everything else — the
    /// empty `SheetPath`, the attributes, the pads and graphics — is the real
    /// message.
    fn board_only_instance(
        kiid: &str,
        reference: &str,
    ) -> copperpour_ipc::gen::kiapi::board::types::FootprintInstance {
        use copperpour_ipc::gen::kiapi;

        let mut instance = kiapi::board::types::FootprintInstance::decode(BOARD_ONLY_CAPTURE)
            .expect("the checked-in KiCad IPC capture must decode");
        instance.id = Some(kiapi::common::types::Kiid {
            value: kiid.to_string(),
        });
        set_field_text(&mut instance.reference_field, "Reference", reference);
        instance
    }

    /// A real `FootprintInstance` for a board-only footprint KiCad left
    /// carrying the library's own default reference, `REF**`.
    ///
    /// The capture above is a mounting hole someone had annotated `MH1`, so it
    /// cannot witness the case #452 actually reported: unannotated graphics,
    /// every one of them called `REF**`, colliding with each other. This one
    /// comes from `copperpour-ipc/tests/fixtures/board_only_shared_reference.kicad_pcb`,
    /// a board KiCad wrote holding two such footprints — and `REF**` is the
    /// library's own string, not one an editing session left behind.
    ///
    /// Regenerate with `COPPERPOUR_CAPTURE_IPC_FIXTURE=1` on the ignored live
    /// test `kicad_reports_the_same_reference_for_two_board_only_footprints`;
    /// provenance is in the fixture's README.
    const SHARED_REFERENCE_CAPTURE: &[u8] =
        include_bytes!("../../tests/fixtures/board_only_shared_reference.ipc.bin");

    /// One of that board's two `REF**` footprints, with only its KIID varied.
    ///
    /// The reference is deliberately *not* varied — it is the shared value the
    /// tests are about, and it is what KiCad really sent. That the board
    /// carries two of them is checked without KiCad by
    /// `the_shared_reference_fixture_still_carries_a_duplicate_board_only_reference`,
    /// so the second instance is a KIID away from the first rather than an
    /// assumption.
    fn ref_star_instance(
        kiid: &str,
    ) -> copperpour_ipc::gen::kiapi::board::types::FootprintInstance {
        use copperpour_ipc::gen::kiapi;

        let mut instance = kiapi::board::types::FootprintInstance::decode(SHARED_REFERENCE_CAPTURE)
            .expect("the checked-in KiCad IPC capture must decode");
        instance.id = Some(kiapi::common::types::Kiid {
            value: kiid.to_string(),
        });
        instance
    }

    /// The same real message, re-labelled as the resistor `resistor()` exports.
    ///
    /// Only the library id and value change: these tests are about the planner
    /// branches an absent identity unlocks, and they need a footprint whose
    /// `footprint_id` and `value` can match a schematic component. The
    /// `not_in_schematic` flag is left exactly as KiCad set it — false — which
    /// is what makes the rename branch reachable at all.
    fn unlinked_instance(
        kiid: &str,
        reference: &str,
    ) -> copperpour_ipc::gen::kiapi::board::types::FootprintInstance {
        use copperpour_ipc::gen::kiapi;

        let mut instance = board_only_instance(kiid, reference);
        if let Some(definition) = instance.definition.as_mut() {
            definition.id = Some(kiapi::common::types::LibraryIdentifier {
                library_nickname: "Resistor_SMD".to_string(),
                entry_name: "R_0603_1608Metric".to_string(),
            });
        }
        set_field_text(&mut instance.value_field, "Value", "10k");
        instance
    }

    #[test]
    fn issue_452_board_only_footprint_reads_as_no_identity_not_a_shared_one() {
        let logo = board_footprint_from_instance(&board_only_instance("logo-kiid", "LOGO1"))
            .expect("a board-only footprint is a valid footprint");

        assert_eq!(
            logo.symbol_path, None,
            "an empty SheetPath means no schematic symbol, not the identity `/`"
        );
        assert_eq!(logo.reference, "LOGO1");
        assert_eq!(logo.kiid, "logo-kiid");
        // KiCad does *not* set `not_in_schematic` on a board-only footprint —
        // it marks it excluded from position files and the BOM instead. The
        // first version of this test asserted the opposite and passed, because
        // the hand-built fixture it ran against said so. That flag is the
        // precondition for the rename branch two tests below, so getting it
        // wrong made that branch unreachable from the tests written to cover
        // this change.
        assert!(
            !logo.not_in_schematic,
            "real KiCad leaves not_in_schematic false on a board-only footprint"
        );
    }

    #[test]
    fn issue_452_two_board_only_footprints_do_not_collide_on_identity() {
        // The whole board as KiCad reports it: one schematic-backed resistor
        // and two pathless graphics. Before the fix both graphics arrived
        // carrying `/`, and the planner refused to sync the resistor.
        let board = board_with(vec![
            board_resistor("R1", Some("/sheet/existing")),
            board_footprint_from_instance(&board_only_instance("logo-kiid", "LOGO1")).unwrap(),
            board_footprint_from_instance(&board_only_instance("fiducial-kiid", "FID1")).unwrap(),
        ]);
        let design = ExportedDesign {
            root_sheet_uuid: None,
            components: vec![resistor("R1", "/sheet/existing")],
            skipped: Vec::new(),
            unassigned: Vec::new(),
            net_names: BTreeSet::new(),
        };

        let plan = plan_sync("netlist", &design, &board);

        assert_eq!(
            plan.diagnostics
                .iter()
                .filter(|diagnostic| diagnostic.code == "duplicate_board_identity")
                .count(),
            0,
            "footprints with no schematic identity are not duplicates of each other"
        );
        assert_eq!(plan.status, PlanStatus::Noop);
        assert_eq!(plan.counts.conflicts.planned, 0);
        assert_eq!(plan.counts.board_only_preserved.planned, 2);
    }

    #[test]
    fn a_schematic_backed_footprint_still_reads_its_identity() {
        use copperpour_ipc::gen::kiapi;

        let mut instance = board_only_instance("r1-kiid", "R1");
        instance.symbol_path = Some(kiapi::common::types::SheetPath {
            path: vec![
                kiapi::common::types::Kiid {
                    value: "sheet-uuid".to_string(),
                },
                kiapi::common::types::Kiid {
                    value: "symbol-uuid".to_string(),
                },
            ],
            path_human_readable: "/Power/".to_string(),
        });

        let footprint = board_footprint_from_instance(&instance).unwrap();

        assert_eq!(
            footprint.symbol_path.as_deref(),
            Some("/sheet-uuid/symbol-uuid"),
            "the guard must not swallow a real schematic identity"
        );
    }
    // The two branches below test `symbol_path.is_none()`, so before #452 they
    // were unreachable for a board-only footprint: every one of them wore the
    // synthetic identity `/`. Reading absence correctly wakes both, which
    // changes plans on boards that never hit the duplicate-identity bug at all.
    // They are pinned here so the consequence is a decision rather than a
    // discovery. Note `board_resistor` leaves `not_in_schematic` false — a
    // footprint KiCad has not flagged — which is what makes them reachable.

    #[test]
    fn issue_452_an_unlinked_footprint_the_schematic_names_is_adopted_not_refused() {
        let design = ExportedDesign {
            root_sheet_uuid: None,
            components: vec![resistor("R1", "/sheet/existing")],
            skipped: Vec::new(),
            unassigned: Vec::new(),
            net_names: BTreeSet::new(),
        };

        let mut footprint =
            board_footprint_from_instance(&unlinked_instance("R1-kiid", "R1")).unwrap();
        footprint.pad_nets = BTreeMap::from([
            ("1".to_string(), "VCC".to_string()),
            ("2".to_string(), "GND".to_string()),
        ]);
        // The capture is a mounting hole dressed as the schematic's resistor,
        // so it is given the resistor's pads along with their nets.
        footprint.pad_numbers = BTreeSet::from(["1".to_string(), "2".to_string()]);
        let plan = plan_sync("netlist", &design, &board_with(vec![footprint]));

        // Was `reference_identity_conflict`: the board footprint appeared to
        // hold the identity `/`, so the schematic's R1 looked like a different
        // symbol wearing the same reference.
        assert_eq!(plan.status, PlanStatus::Ready);
        assert_eq!(plan.counts.updated.planned, 1);
        assert_eq!(plan.counts.board_only_preserved.planned, 0);
        let PlannedChange::Update {
            kiid, symbol_path, ..
        } = &plan.changes[0]
        else {
            panic!("adoption is an update, not an add: {:?}", plan.changes[0]);
        };
        assert_eq!(kiid, "R1-kiid");
        assert_eq!(
            symbol_path, "/sheet/existing",
            "adoption writes the schematic identity onto a footprint the \
             planner previously refused to touch"
        );
    }

    #[test]
    fn issue_452_an_unlinked_lookalike_blocks_a_new_component_instead_of_duplicating_it() {
        // The board carries a footprint with no schematic identity whose
        // library id and value match a component the schematic has just gained.
        let design = ExportedDesign {
            root_sheet_uuid: None,
            components: vec![resistor("R5", "/sheet/new")],
            skipped: Vec::new(),
            unassigned: Vec::new(),
            net_names: BTreeSet::new(),
        };

        let plan = plan_sync(
            "netlist",
            &design,
            &board_with(vec![board_footprint_from_instance(&unlinked_instance(
                "R1-kiid", "R1",
            ))
            .unwrap()]),
        );

        // This plan was `Ready` before #452, and it added a second identical
        // footprint beside the unlinked one. It is now the conflict the
        // `possible_renames` scan was written to raise. Better, but it is a
        // fix that can newly block a sync, and that belongs in the notes.
        assert_eq!(plan.status, PlanStatus::Conflict);
        assert_eq!(plan.counts.added.planned, 0);
        assert!(plan
            .diagnostics
            .iter()
            .any(|diagnostic| diagnostic.code == "reference_only_rename_ambiguous"));
    }
    #[test]
    fn issue_452_repeated_ref_star_graphics_are_not_a_duplicate_the_schematic_can_see() {
        // The reported board: a synchronized resistor plus two unannotated
        // graphics, both called `REF**` because that is what the library calls
        // them. Nothing in the schematic is named `REF**`, so no adoption ever
        // looks that reference up and there is nothing to disambiguate.
        let board = board_with(vec![
            board_resistor("R1", Some("/sheet/existing")),
            board_footprint_from_instance(&ref_star_instance("first-kiid")).unwrap(),
            board_footprint_from_instance(&ref_star_instance("second-kiid")).unwrap(),
        ]);
        let design = ExportedDesign {
            root_sheet_uuid: None,
            components: vec![resistor("R1", "/sheet/existing")],
            skipped: Vec::new(),
            unassigned: Vec::new(),
            net_names: BTreeSet::new(),
        };

        let plan = plan_sync("netlist", &design, &board);

        assert_eq!(
            plan.diagnostics
                .iter()
                .filter(|diagnostic| diagnostic.code == "duplicate_board_reference")
                .count(),
            0,
            "a reference the schematic never names has no adoption to make ambiguous"
        );
        assert_eq!(plan.status, PlanStatus::Noop);
        assert_eq!(plan.counts.conflicts.planned, 0);
        assert_eq!(
            plan.counts.board_only_preserved.planned, 2,
            "both graphics are preserved, not merely un-diagnosed"
        );
        assert!(plan.changes.is_empty());
    }

    #[test]
    fn issue_452_repeated_ref_star_graphics_no_longer_block_an_unrelated_update() {
        // The same board, with the resistor out of date. Before this change the
        // duplicate `REF**` diagnostic conflicted the whole plan, so the update
        // the user asked for could not be applied while those graphics existed
        // — which is the symptom #452 was filed about.
        let mut stale = board_resistor("R1", Some("/sheet/existing"));
        stale.value = "4k7".to_string();
        let board = board_with(vec![
            stale,
            board_footprint_from_instance(&ref_star_instance("first-kiid")).unwrap(),
            board_footprint_from_instance(&ref_star_instance("second-kiid")).unwrap(),
        ]);
        let design = ExportedDesign {
            root_sheet_uuid: None,
            components: vec![resistor("R1", "/sheet/existing")],
            skipped: Vec::new(),
            unassigned: Vec::new(),
            net_names: BTreeSet::new(),
        };

        let plan = plan_sync("netlist", &design, &board);

        assert_eq!(plan.status, PlanStatus::Ready);
        assert_eq!(plan.counts.updated.planned, 1);
        assert_eq!(plan.counts.board_only_preserved.planned, 2);
        let Some(PlannedChange::Update { kiid, value, .. }) = plan.changes.first() else {
            panic!("expected one update, got {:?}", plan.changes);
        };
        assert_eq!(kiid, "R1-kiid");
        assert_eq!(value, "10k");
    }

    /// A **scope test**: it passes with the narrowing removed, and that is the
    /// point of it.
    ///
    /// The guard tests above fail when the `exported_references` clause is
    /// deleted. This one cannot — it asserts the diagnostic that the
    /// unconditional version also raises. It exists to prove the narrowing did
    /// not go too far, so its silence under neutering is the finding, and
    /// counting it as coverage of the change would be a lie.
    #[test]
    fn issue_452_a_duplicate_reference_the_schematic_does_use_is_still_a_conflict() {
        // Two pathless footprints called `R1`, and a schematic component called
        // `R1` with no board identity to match on. The reference-only adoption
        // path keys on exactly that name, so with two candidates it would pick
        // whichever one `board_by_reference` happened to keep and write the
        // schematic identity onto it. That is the ambiguity the diagnostic
        // exists for, and scoping the rule to the schematic's own reference set
        // — rather than exempting board-only footprints as a kind — is what
        // keeps it.
        let board = board_with(vec![
            board_footprint_from_instance(&unlinked_instance("first-kiid", "R1")).unwrap(),
            board_footprint_from_instance(&unlinked_instance("second-kiid", "R1")).unwrap(),
        ]);
        let design = ExportedDesign {
            root_sheet_uuid: None,
            components: vec![resistor("R1", "/sheet/existing")],
            skipped: Vec::new(),
            unassigned: Vec::new(),
            net_names: BTreeSet::new(),
        };

        let plan = plan_sync("netlist", &design, &board);

        assert_eq!(plan.status, PlanStatus::Conflict);
        assert!(plan
            .diagnostics
            .iter()
            .any(|diagnostic| diagnostic.code == "duplicate_board_reference"));
        assert!(
            plan.changes.is_empty(),
            "an ambiguous adoption target must not be written to: {:?}",
            plan.changes
        );
        assert_eq!(plan.counts.updated.planned, 0);
        assert_eq!(plan.counts.added.planned, 0);
    }

    /// The `on_board=no` side of the export, which is a consult site too.
    ///
    /// One of the four `board_by_reference` lookups reads `skipped.reference`,
    /// so the set has to span both halves of the export or an excluded
    /// instance loses its ambiguity check. This test guards the `.chain(...)`
    /// specifically: it survives deleting the whole narrowing — the
    /// unconditional diagnostic raises it too — and fails only when the
    /// skipped half is dropped from the set. Do not read it as coverage of the
    /// narrowing itself; the two `ref_star` tests above are that.
    #[test]
    fn issue_452_a_duplicate_reference_only_an_on_board_no_instance_uses_still_conflicts() {
        let board = board_with(vec![
            board_footprint_from_instance(&unlinked_instance("first-kiid", "R9")).unwrap(),
            board_footprint_from_instance(&unlinked_instance("second-kiid", "R9")).unwrap(),
        ]);
        let design = ExportedDesign {
            root_sheet_uuid: None,
            components: vec![resistor("R1", "/sheet/existing")],
            skipped: vec![SkippedComponent {
                reference: "R9".to_string(),
                symbol_path: "/sheet/excluded".to_string(),
            }],
            unassigned: Vec::new(),
            net_names: BTreeSet::new(),
        };

        let plan = plan_sync("netlist", &design, &board);

        assert_eq!(plan.status, PlanStatus::Conflict);
        assert!(plan
            .diagnostics
            .iter()
            .any(|diagnostic| diagnostic.code == "duplicate_board_reference"));
    }
    /// One of a duplicate pair *can* still be written to — when the schematic
    /// matched it by path, which the duplicate reference plays no part in.
    ///
    /// A footprint can carry a schematic identity and still wear `REF**` if it
    /// was never back-annotated, and then it collides with the board's loose
    /// graphics. The old diagnostic blocked the whole sync, so the identity
    /// match could not be applied while such a graphic existed; now the
    /// path-matched footprint is renamed and the other is preserved.
    ///
    /// This is the honest limit of "un-diagnosed footprints are left alone":
    /// they are never *selected by reference*, because the reference is not in
    /// the export. Selection by path is a different and unambiguous route.
    #[test]
    fn issue_452_a_path_matched_footprint_is_still_adopted_despite_a_duplicate_reference() {
        let mut pathed = board_footprint_from_instance(&ref_star_instance("pathed-kiid")).unwrap();
        pathed.symbol_path = Some("/sheet/existing".to_string());
        pathed.footprint_id = "Resistor_SMD:R_0603_1608Metric".to_string();
        pathed.value = "10k".to_string();
        // A captured graphic dressed as the schematic's resistor, so it is
        // given the resistor's pads as well as its identity.
        pathed.pad_numbers = BTreeSet::from(["1".to_string(), "2".to_string()]);
        let board = board_with(vec![
            pathed,
            board_footprint_from_instance(&ref_star_instance("loose-kiid")).unwrap(),
        ]);
        let design = ExportedDesign {
            root_sheet_uuid: None,
            components: vec![resistor("R1", "/sheet/existing")],
            skipped: Vec::new(),
            unassigned: Vec::new(),
            net_names: BTreeSet::new(),
        };

        let plan = plan_sync("netlist", &design, &board);

        assert_eq!(plan.status, PlanStatus::Ready);
        let Some(PlannedChange::Update {
            kiid, reference, ..
        }) = plan.changes.first()
        else {
            panic!("expected one update, got {:?}", plan.changes);
        };
        assert_eq!(
            kiid, "pathed-kiid",
            "the path match selected it, not the reference"
        );
        assert_eq!(reference, "R1");
        assert_eq!(
            plan.counts.board_only_preserved.planned, 1,
            "the footprint the schematic never named is preserved untouched"
        );
    }

    /// R1 linked to its symbol, R3 whose symbol left the schematic, and MH1,
    /// which has no symbol and the board-only attribute (#861).
    fn footprints_without_symbols() -> (ExportedDesign, BoardState) {
        let mut mounting_hole = board_resistor("MH1", None);
        mounting_hole.not_in_schematic = true;
        let board = board_with(vec![
            board_resistor("R1", Some("/sheet/existing")),
            board_resistor("R3", Some("/sheet/removed")),
            mounting_hole,
        ]);
        let design = ExportedDesign {
            root_sheet_uuid: None,
            components: vec![resistor("R1", "/sheet/existing")],
            skipped: Vec::new(),
            unassigned: Vec::new(),
            net_names: BTreeSet::new(),
        };
        (design, board)
    }

    fn r3_without_a_symbol(locked: bool) -> FootprintWithoutSymbol {
        FootprintWithoutSymbol {
            reference: "R3".to_string(),
            kiid: "R3-kiid".to_string(),
            footprint_id: "Resistor_SMD:R_0603_1608Metric".to_string(),
            locked,
            reason: WithoutSymbolReason::SymbolRemoved,
        }
    }

    fn r3_deleted() -> PlannedChange {
        PlannedChange::Delete {
            kiid: "R3-kiid".to_string(),
            reference: "R3".to_string(),
            footprint_id: "Resistor_SMD:R_0603_1608Metric".to_string(),
            reason: WithoutSymbolReason::SymbolRemoved,
        }
    }

    /// Without `delete_footprints_without_symbols`, R3 is kept and named, and
    /// MH1 is kept and not named; both are counted as preserved.
    #[test]
    fn a_footprint_without_a_symbol_is_named_and_kept() {
        let (design, board) = footprints_without_symbols();

        let plan = plan_sync("netlist", &design, &board);

        assert_eq!(plan.status, PlanStatus::Noop);
        assert!(plan.changes.is_empty(), "{:?}", plan.changes);
        assert_eq!(plan.without_symbols, vec![r3_without_a_symbol(false)]);
        assert_eq!(plan.counts.board_only_preserved.planned, 2);
        assert_eq!(plan.counts.deleted.planned, 0);
        assert!(plan.diagnostics.is_empty(), "{:?}", plan.diagnostics);
    }

    /// With it, R3 is deleted, and that alone makes the plan ready. MH1 has
    /// the board-only attribute, so it stays, as KiCad keeps it.
    #[test]
    fn a_footprint_without_a_symbol_is_deleted_on_request() {
        let (design, board) = footprints_without_symbols();

        let plan = plan_sync_acknowledging("netlist", &design, &board, &BTreeSet::new(), true);

        assert_eq!(plan.status, PlanStatus::Ready);
        assert_eq!(plan.changes, vec![r3_deleted()]);
        assert_eq!(plan.counts.deleted.planned, 1);
        assert_eq!(plan.counts.board_only_preserved.planned, 1);
        assert!(
            plan.without_symbols.is_empty(),
            "{:?}",
            plan.without_symbols
        );
        assert!(plan.diagnostics.is_empty(), "{:?}", plan.diagnostics);
    }

    /// A locked R3 is kept with a note that refuses nothing, as KiCad keeps it
    /// with a warning when locks are not overridden.
    #[test]
    fn a_locked_footprint_without_a_symbol_is_kept_with_a_note() {
        let (design, mut board) = footprints_without_symbols();
        board.footprints[1].locked = true;

        let plan = plan_sync_acknowledging("netlist", &design, &board, &BTreeSet::new(), true);

        assert_eq!(plan.status, PlanStatus::Noop);
        assert!(plan.changes.is_empty(), "{:?}", plan.changes);
        assert_eq!(plan.without_symbols, vec![r3_without_a_symbol(true)]);
        assert_eq!(plan.counts.board_only_preserved.planned, 2);
        assert_eq!(plan.counts.conflicts.planned, 0);
        assert_eq!(plan.diagnostics.len(), 1, "{:?}", plan.diagnostics);
        assert_eq!(plan.diagnostics[0].code, "footprint_locked_not_deleted");
        assert_eq!(plan.diagnostics[0].reference.as_deref(), Some("R3"));
    }

    /// A plan with a conflict deletes nothing: R3 is named as kept.
    #[test]
    fn a_plan_with_a_conflict_deletes_no_footprint() {
        let (mut design, board) = footprints_without_symbols();
        design.components.push(resistor("R1", "/sheet/other"));

        let plan = plan_sync_acknowledging("netlist", &design, &board, &BTreeSet::new(), true);

        assert_eq!(plan.status, PlanStatus::Conflict);
        assert!(plan.changes.is_empty(), "{:?}", plan.changes);
        assert_eq!(plan.counts.deleted.planned, 0);
        assert_eq!(plan.without_symbols, vec![r3_without_a_symbol(false)]);
        assert_eq!(plan.counts.board_only_preserved.planned, 2);
    }

    /// A plan the handler refuses later, as a stale revision or a preflight
    /// refusal, keeps the footprints it would have deleted, and says so.
    #[test]
    fn a_refused_plan_lists_the_footprints_it_would_have_deleted() {
        let (design, board) = footprints_without_symbols();
        let mut plan = plan_sync_acknowledging("netlist", &design, &board, &BTreeSet::new(), true);

        refuse_plan(
            &mut plan,
            vec![conflict("preflight_conflict", "refused".to_string(), None)],
        );

        assert!(plan.changes.is_empty(), "{:?}", plan.changes);
        assert_eq!(plan.counts.deleted.planned, 0);
        assert_eq!(plan.without_symbols, vec![r3_without_a_symbol(false)]);
        assert_eq!(plan.counts.board_only_preserved.planned, 2);
    }

    /// A part the preflight skips takes no deletion with it, and a plan left
    /// with only the deletion still applies it.
    #[test]
    fn a_skipped_part_leaves_the_deletions_planned() {
        let (mut design, board) = footprints_without_symbols();
        design.components.push(resistor("R2", "/sheet/new"));
        let mut plan = plan_sync_acknowledging("netlist", &design, &board, &BTreeSet::new(), true);
        assert_eq!(plan.counts.added.planned, 1, "{:?}", plan.changes);

        let applies = skip_parts(
            &mut plan,
            vec![cannot_build("unsupported_library_footprint", &["R2"])],
            &board,
            &BTreeSet::new(),
        );

        assert!(applies);
        assert_eq!(plan.status, PlanStatus::Ready);
        assert_eq!(plan.changes, vec![r3_deleted()]);
        assert_eq!(plan.counts.added.planned, 0);
        assert_eq!(plan.counts.deleted.planned, 1);
    }

    /// A via or copper zone on the planner's board (#862).
    fn copper(kind: CopperKind, kiid: &str, net: &str) -> CopperItem {
        CopperItem {
            kind,
            kiid: kiid.to_string(),
            net: net.to_string(),
            name: String::new(),
        }
    }

    /// R1 and R2 on the board, pad 1 on VCC and pad 2 on GND; the schematic
    /// moves R1's pad 2 to `r1_to` and R2's to `r2_to`, and has no GND.
    fn plan_moving_gnd(r1_to: &str, r2_to: &str) -> (SyncPlan, BoardState, BTreeSet<String>) {
        let board = board_with(vec![
            board_resistor("R1", Some("/r1")),
            board_resistor("R2", Some("/r2")),
        ]);
        let mut r1 = resistor("R1", "/r1");
        r1.pad_nets.insert("2".to_string(), r1_to.to_string());
        let mut r2 = resistor("R2", "/r2");
        r2.pad_nets.insert("2".to_string(), r2_to.to_string());
        let net_names = ["VCC", r1_to, r2_to]
            .into_iter()
            .map(str::to_string)
            .collect::<BTreeSet<_>>();
        let design = ExportedDesign {
            root_sheet_uuid: None,
            components: vec![r1, r2],
            skipped: Vec::new(),
            unassigned: Vec::new(),
            net_names: net_names.clone(),
        };
        let plan = plan_sync("netlist", &design, &board);
        assert_eq!(plan.status, PlanStatus::Ready, "{:?}", plan.diagnostics);
        (plan, board, net_names)
    }

    fn moved(kind: CopperKind, kiid: &str, from: &str, to: &str) -> CopperNetChange {
        CopperNetChange {
            kind,
            kiid: kiid.to_string(),
            name: String::new(),
            from: from.to_string(),
            to: to.to_string(),
        }
    }

    fn note_codes(plan: &SyncPlan) -> Vec<(&str, Option<&str>)> {
        plan.diagnostics
            .iter()
            .map(|note| (note.code.as_str(), note.kiid.as_deref()))
            .collect()
    }

    /// The via and the zone on GND, which leaves the schematic, follow the
    /// pads off it; copper on VCC, still in the schematic, and on no net
    /// stays without a note.
    #[test]
    fn the_vias_and_zones_of_a_net_that_left_follow_its_pads() {
        let (mut plan, board, net_names) = plan_moving_gnd("NEW", "NEW");
        let copper = [
            copper(CopperKind::Via, "via-gnd", "GND"),
            copper(CopperKind::Via, "via-vcc", "VCC"),
            copper(CopperKind::Via, "via-none", ""),
            copper(CopperKind::Zone, "zone-gnd", "GND"),
        ];

        plan_copper_nets(&mut plan, &board, &copper, &net_names, &BTreeMap::new());

        assert_eq!(
            plan.copper_net_changes,
            vec![
                moved(CopperKind::Via, "via-gnd", "GND", "NEW"),
                moved(CopperKind::Zone, "zone-gnd", "GND", "NEW"),
            ]
        );
        assert_eq!(plan.counts.copper_net_changes.planned, 2);
        assert_eq!(note_codes(&plan), []);
    }

    /// GND splits: R1's pad goes to A, R2's to B. As in KiCad, the last pad
    /// sets the net map, so the via and the zone with no connected pad go to
    /// B; the zone R1's pad touches goes to A.
    #[test]
    fn a_zone_takes_the_net_of_a_pad_connected_to_it_first() {
        let (mut plan, board, net_names) = plan_moving_gnd("A", "B");
        let copper = [
            copper(CopperKind::Via, "via", "GND"),
            copper(CopperKind::Zone, "zone-r1", "GND"),
            copper(CopperKind::Zone, "zone-alone", "GND"),
        ];
        let zone_pads = BTreeMap::from([(
            "zone-r1".to_string(),
            vec![("R1-kiid".to_string(), "2".to_string())],
        )]);

        plan_copper_nets(&mut plan, &board, &copper, &net_names, &zone_pads);

        assert_eq!(
            plan.copper_net_changes,
            vec![
                moved(CopperKind::Via, "via", "GND", "B"),
                moved(CopperKind::Zone, "zone-r1", "GND", "A"),
                moved(CopperKind::Zone, "zone-alone", "GND", "B"),
            ]
        );
    }

    /// A pad connected to the zone that stays on the zone's own net is passed
    /// over, as KiCad's `if( getNetname( pad ) != zone->GetNetname() )` passes
    /// it (`board_netlist_updater.cpp:1786`, 10.0.8): H1, a footprint the
    /// schematic does not touch (a mounting hole or fiducial), keeps its pad 2
    /// on GND under the GND zone. Listed before R1's pad, the zone still takes
    /// R1's new net; listed alone, the net map gives the net (#866 review).
    #[test]
    fn a_connected_pad_that_stays_on_the_zones_net_is_passed_over() {
        let (mut plan, mut board, net_names) = plan_moving_gnd("A", "B");
        board.footprints.push(board_resistor("H1", None));
        let copper = [
            copper(CopperKind::Zone, "zone-h1-first", "GND"),
            copper(CopperKind::Zone, "zone-h1-only", "GND"),
        ];
        let h1 = ("H1-kiid".to_string(), "2".to_string());
        let r1 = ("R1-kiid".to_string(), "2".to_string());
        let zone_pads = BTreeMap::from([
            ("zone-h1-first".to_string(), vec![h1.clone(), r1]),
            ("zone-h1-only".to_string(), vec![h1]),
        ]);

        plan_copper_nets(&mut plan, &board, &copper, &net_names, &zone_pads);

        assert_eq!(
            plan.copper_net_changes,
            vec![
                moved(CopperKind::Zone, "zone-h1-first", "GND", "A"),
                moved(CopperKind::Zone, "zone-h1-only", "GND", "B"),
            ]
        );
    }

    /// KiCad records a pad's move in the net map only when its new net is
    /// not empty: R2's pad 2 losing its net does not take the via off to no
    /// net, and R1's pad 2 moving to A still leads it there.
    #[test]
    fn a_pad_that_loses_its_net_leaves_the_net_map_alone() {
        let (mut plan, board, net_names) = plan_moving_gnd("A", "");
        let copper = [copper(CopperKind::Via, "via", "GND")];

        plan_copper_nets(&mut plan, &board, &copper, &net_names, &BTreeMap::new());

        assert_eq!(
            plan.copper_net_changes,
            vec![moved(CopperKind::Via, "via", "GND", "A")]
        );
    }

    /// Copper on a net that left the schematic, with no pad moving off it,
    /// stays, each item with KiCad's warning as a note naming it.
    #[test]
    fn copper_on_a_net_no_pad_moves_off_stays_with_a_note() {
        let (mut plan, board, net_names) = plan_moving_gnd("NEW", "NEW");
        let copper = [
            copper(CopperKind::Via, "via-old", "OLD"),
            copper(CopperKind::Zone, "zone-old", "OLD"),
        ];

        plan_copper_nets(&mut plan, &board, &copper, &net_names, &BTreeMap::new());

        assert_eq!(plan.copper_net_changes, []);
        assert_eq!(
            note_codes(&plan),
            [
                ("via_net_not_in_schematic", Some("via-old")),
                ("zone_net_not_in_schematic", Some("zone-old")),
            ]
        );
        assert_eq!(plan.status, PlanStatus::Ready);
    }

    /// KiCad reads a zone net that starts with a schematic net's name as
    /// that net (`isInNetlist`); a via gets no such reading.
    #[test]
    fn a_zone_on_a_pseudo_name_of_a_schematic_net_stays() {
        let (mut plan, board, net_names) = plan_moving_gnd("NEW", "NEW");
        let copper = [
            copper(CopperKind::Zone, "zone", "NEW_1"),
            copper(CopperKind::Via, "via", "NEW_1"),
        ];

        plan_copper_nets(&mut plan, &board, &copper, &net_names, &BTreeMap::new());

        assert_eq!(plan.copper_net_changes, []);
        assert_eq!(
            note_codes(&plan),
            [("via_net_not_in_schematic", Some("via"))]
        );
    }

    /// The net map comes from the pad changes still planned: with the
    /// updates taken off, as the preflight takes off a skipped part's, no
    /// copper moves.
    #[test]
    fn copper_follows_only_the_pad_changes_still_planned() {
        let (mut plan, board, net_names) = plan_moving_gnd("NEW", "NEW");
        plan.changes.clear();
        let copper = [copper(CopperKind::Via, "via", "GND")];

        plan_copper_nets(&mut plan, &board, &copper, &net_names, &BTreeMap::new());

        assert_eq!(plan.copper_net_changes, []);
        assert_eq!(
            note_codes(&plan),
            [("via_net_not_in_schematic", Some("via"))]
        );
    }

    /// A plan with a conflict moves no copper and notes none; a plan refused
    /// later drops the moves it planned.
    #[test]
    fn a_refused_plan_moves_no_copper() {
        let copper = [copper(CopperKind::Via, "via", "GND")];
        let (mut conflicted, board, net_names) = plan_moving_gnd("NEW", "NEW");
        conflicted.status = PlanStatus::Conflict;
        plan_copper_nets(
            &mut conflicted,
            &board,
            &copper,
            &net_names,
            &BTreeMap::new(),
        );
        assert_eq!(conflicted.copper_net_changes, []);
        assert_eq!(note_codes(&conflicted), []);

        let (mut plan, board, net_names) = plan_moving_gnd("NEW", "NEW");
        plan_copper_nets(&mut plan, &board, &copper, &net_names, &BTreeMap::new());
        assert_eq!(plan.counts.copper_net_changes.planned, 1);
        refuse_plan(
            &mut plan,
            vec![conflict("preflight_conflict", "refused".to_string(), None)],
        );
        assert_eq!(plan.copper_net_changes, []);
        assert_eq!(plan.counts.copper_net_changes.planned, 0);
    }

    /// The parser keeps every net a pin is on, a footprint-less part's
    /// included, as KiCad's netlist holds it.
    #[test]
    fn the_netlist_keeps_the_net_of_a_part_with_no_footprint() {
        let design = parse_exported_netlist(UNASSIGNED).unwrap();
        assert!(!design.net_names.is_empty());
        let footprint_nets = design
            .components
            .iter()
            .flat_map(|component| component.pad_nets.values().cloned())
            .collect::<BTreeSet<_>>();
        assert!(
            design.net_names.is_superset(&footprint_nets),
            "{:?} lacks some of {footprint_nets:?}",
            design.net_names
        );
        // R1 has no footprint; its pad 2's net is on no other pin.
        assert!(
            design.net_names.contains("unconnected-(R1-Pad2)"),
            "{:?}",
            design.net_names
        );
        assert!(!footprint_nets.contains("unconnected-(R1-Pad2)"));
    }

    /// KiCad's own `R1` (`issue_474_r1.ipc.bin`: pad 1 on `VCC`, pad 2 on
    /// `GND`), bound to the `/Power/` symbol that [`ONE_RESISTOR`] exports.
    fn schematic_backed_resistor() -> prost_types::Any {
        use copperpour_ipc::gen::kiapi;
        use prost::Message;
        const CAPTURE: &[u8] = include_bytes!("../../tests/fixtures/issue_474_r1.ipc.bin");
        let mut footprint = kiapi::board::types::FootprintInstance::decode(CAPTURE)
            .expect("the checked-in KiCad IPC capture must decode");
        footprint.symbol_path = Some(kiapi::common::types::SheetPath {
            path: vec![
                kiapi::common::types::Kiid {
                    value: "sheet-uuid".to_string(),
                },
                kiapi::common::types::Kiid {
                    value: "symbol-uuid".to_string(),
                },
            ],
            path_human_readable: "/Power/".to_string(),
        });
        set_field_text(&mut footprint.value_field, "Value", "1k");
        copperpour_ipc::builders::pack_any(&footprint, "kiapi.board.types.FootprintInstance")
    }

    /// KiCad's captured `GND` copper pour and a rule area (`issue_474_ipc.README.md`).
    const GND_POUR: &[u8] = include_bytes!("../../tests/fixtures/issue_474_copper_zone_0.ipc.bin");
    const RULE_AREA: &[u8] = include_bytes!("../../tests/fixtures/issue_474_zone_0.ipc.bin");

    /// A zone as KiCad lists it: `bytes` is the `Zone` message.
    fn zone_item(bytes: &[u8]) -> prost_types::Any {
        prost_types::Any {
            type_url: "type.googleapis.com/kiapi.board.types.Zone".to_string(),
            value: bytes.to_vec(),
        }
    }

    /// The captured pour with its net taken out.
    fn pour_without_a_net() -> prost_types::Any {
        use copperpour_ipc::gen::kiapi::board::types::{zone::Settings, Zone};
        use prost::Message;
        let mut zone = Zone::decode(GND_POUR).expect("the captured pour decodes");
        let Some(Settings::CopperSettings(copper)) = zone.settings.as_mut() else {
            panic!("the captured pour is a copper zone");
        };
        copper.net = None;
        copperpour_ipc::builders::pack_any(&zone, "kiapi.board.types.Zone")
    }

    /// The captured pour on `net` instead of `GND`.
    fn pour_on(net: &str) -> prost_types::Any {
        use copperpour_ipc::gen::kiapi::board::types::{zone::Settings, Zone};
        use prost::Message;
        let mut zone = Zone::decode(GND_POUR).expect("the captured pour decodes");
        let Some(Settings::CopperSettings(copper)) = zone.settings.as_mut() else {
            panic!("the captured pour is a copper zone");
        };
        copper.net.as_mut().expect("the captured pour's net").name = net.to_string();
        copperpour_ipc::builders::pack_any(&zone, "kiapi.board.types.Zone")
    }

    /// A complete #474 sync through the registered tool, not just the pure
    /// planner. The mock speaks KiCad's real protobuf protocol and retains the
    /// live board between requests, so the assertions below are an independent
    /// readback of what the apply actually left behind.
    #[tokio::test]
    async fn issue_474_apply_preserves_every_board_only_object() {
        use crate::router::ToolRouter;
        use crate::tools::cli::test_support::write_script;
        use crate::tools::{ServerConfig, ToolContext};
        use copperpour_ipc::gen::kiapi;
        use prost::Message;
        use std::sync::{Arc, Mutex};

        #[derive(Clone)]
        struct MockBoard {
            footprints: Arc<Mutex<Vec<prost_types::Any>>>,
            zones: Arc<Mutex<Vec<prost_types::Any>>>,
        }

        fn ok_item_status() -> kiapi::common::commands::ItemStatus {
            kiapi::common::commands::ItemStatus {
                code: kiapi::common::commands::ItemStatusCode::IscOk as i32,
                error_message: String::new(),
            }
        }

        let directory = tempfile::tempdir().unwrap();
        let schematic = directory.path().join("preserve.kicad_sch");
        let board = directory.path().join("preserve.kicad_pcb");
        let exported = directory.path().join("preserve.net");
        std::fs::write(
            &schematic,
            include_bytes!("../../tests/fixtures/structural_scans_kicad10.kicad_sch"),
        )
        .unwrap();
        std::fs::write(
            &board,
            include_bytes!("../../tests/fixtures/specctra_two_resistors.kicad_pcb"),
        )
        .unwrap();
        std::fs::write(
            &exported,
            ONE_RESISTOR
                .replace("Resistor_SMD:R_0603_1608Metric", "Resistor_SMD:R_0402")
                .replace("/Power/VCC", "VCC"),
        )
        .unwrap();

        let unix_source = exported.to_string_lossy().replace('\'', "'\\''");
        let windows_source = exported.to_string_lossy();
        let cli = write_script(
            directory.path(),
            "fake-kicad-cli-sync",
            &format!(
                "#!/bin/sh\nwhile [ \"$#\" -gt 0 ]; do\n  if [ \"$1\" = \"--output\" ]; then\n    shift\n    cp '{unix_source}' \"$1\"\n    exit $?\n  fi\n  shift\ndone\nexit 2\n"
            ),
            &format!(
                "@echo off\r\n:loop\r\nif \"%~1\"==\"\" exit /b 2\r\nif \"%~1\"==\"--output\" goto found\r\nshift\r\ngoto loop\r\n:found\r\nshift\r\ncopy /Y \"{windows_source}\" \"%~1\" >nul\r\nexit /b %ERRORLEVEL%\r\n"
            ),
        );

        let schematic_backed = schematic_backed_resistor();
        let logo = copperpour_ipc::builders::pack_any(
            &board_only_instance("logo-live", "REF**"),
            "kiapi.board.types.FootprintInstance",
        );
        let fiducial = copperpour_ipc::builders::pack_any(
            &board_only_instance("fiducial-live", "REF**"),
            "kiapi.board.types.FootprintInstance",
        );
        let copper_zone = zone_item(GND_POUR);
        let keepout = zone_item(RULE_AREA);
        let copper_kind = kiapi::board::types::Zone::decode(copper_zone.value.as_slice())
            .expect("captured copper zone");
        let keepout_kind = kiapi::board::types::Zone::decode(keepout.value.as_slice())
            .expect("captured rule area");
        assert_eq!(
            copper_kind.r#type,
            kiapi::board::types::ZoneType::ZtCopper as i32
        );
        assert_eq!(
            keepout_kind.r#type,
            kiapi::board::types::ZoneType::ZtRuleArea as i32
        );
        let state = MockBoard {
            footprints: Arc::new(Mutex::new(vec![schematic_backed, logo, fiducial])),
            zones: Arc::new(Mutex::new(vec![copper_zone, keepout])),
        };
        let footprints_before = state.footprints.lock().unwrap().clone();
        let zones_before = state.zones.lock().unwrap().clone();
        let responder_state = state.clone();

        let server = crate::tools::pcb_board::board_mock::spawn_kicad_holding_board(
            &board,
            move |command| {
                if command.type_url.ends_with("SaveDocumentToString") {
                    let request = kiapi::common::commands::SaveDocumentToString::decode(
                        command.value.as_slice(),
                    )
                    .expect("snapshot request");
                    return Some(copperpour_ipc::builders::pack_any(
                        &kiapi::common::commands::SavedDocumentResponse {
                            document: request.document,
                            contents: include_str!(
                                "../../tests/fixtures/specctra_two_resistors.kicad_pcb"
                            )
                            .into(),
                        },
                        "kiapi.common.commands.SavedDocumentResponse",
                    ));
                }
                if command.type_url.ends_with("GetItems") {
                    let request =
                        kiapi::common::commands::GetItems::decode(command.value.as_slice())
                            .expect("GetItems request");
                    let requested = request.types.first().copied().unwrap_or_default();
                    let items = match kiapi::common::types::KiCadObjectType::try_from(requested) {
                        Ok(kiapi::common::types::KiCadObjectType::KotPcbFootprint) => {
                            responder_state.footprints.lock().unwrap().clone()
                        }
                        Ok(kiapi::common::types::KiCadObjectType::KotPcbZone) => {
                            responder_state.zones.lock().unwrap().clone()
                        }
                        _ => Vec::new(),
                    };
                    return Some(copperpour_ipc::builders::pack_any(
                        &kiapi::common::commands::GetItemsResponse {
                            header: None,
                            status: kiapi::common::types::ItemRequestStatus::IrsOk as i32,
                            items,
                        },
                        "kiapi.common.commands.GetItemsResponse",
                    ));
                }
                if command.type_url.ends_with("GetNets") {
                    return Some(copperpour_ipc::builders::pack_any(
                        &kiapi::board::commands::NetsResponse {
                            nets: vec![
                                kiapi::board::types::Net {
                                    code: Some(kiapi::board::types::NetCode { value: 1 }),
                                    name: "/Power/VCC".to_string(),
                                },
                                kiapi::board::types::Net {
                                    code: Some(kiapi::board::types::NetCode { value: 2 }),
                                    name: "GND".to_string(),
                                },
                            ],
                        },
                        "kiapi.board.commands.NetsResponse",
                    ));
                }
                if command.type_url.ends_with("GetBoundingBox") {
                    return Some(crate::tools::pcb_board::board_mock::kicad_bounding_boxes(
                        command,
                        |_| (0.0, 0.0, 50.0, 40.0),
                    ));
                }
                if command.type_url.ends_with("BeginCommit") {
                    return Some(copperpour_ipc::builders::pack_any(
                        &kiapi::common::commands::BeginCommitResponse {
                            id: Some(kiapi::common::types::Kiid {
                                value: "commit-474".to_string(),
                            }),
                        },
                        "kiapi.common.commands.BeginCommitResponse",
                    ));
                }
                if command.type_url.ends_with("UpdateItems") {
                    let request =
                        kiapi::common::commands::UpdateItems::decode(command.value.as_slice())
                            .expect("UpdateItems request");
                    let mut board_footprints = responder_state.footprints.lock().unwrap();
                    let mut updated_items = Vec::new();
                    for updated in request.items {
                        let updated_fp = kiapi::board::types::FootprintInstance::decode(
                            updated.value.as_slice(),
                        )
                        .expect("updated footprint");
                        let updated_id =
                            updated_fp.id.as_ref().expect("updated KIID").value.clone();
                        let position = board_footprints
                            .iter()
                            .position(|item| {
                                kiapi::board::types::FootprintInstance::decode(
                                    item.value.as_slice(),
                                )
                                .ok()
                                .and_then(|fp| fp.id)
                                .is_some_and(|id| id.value == updated_id)
                            })
                            .expect("existing update target");
                        board_footprints[position] = updated.clone();
                        updated_items.push(kiapi::common::commands::ItemUpdateResult {
                            status: Some(ok_item_status()),
                            item: Some(updated),
                        });
                    }
                    return Some(copperpour_ipc::builders::pack_any(
                        &kiapi::common::commands::UpdateItemsResponse {
                            header: None,
                            status: kiapi::common::types::ItemRequestStatus::IrsOk as i32,
                            updated_items,
                        },
                        "kiapi.common.commands.UpdateItemsResponse",
                    ));
                }
                if command.type_url.ends_with("EndCommit") {
                    return Some(copperpour_ipc::builders::pack_any(
                        &kiapi::common::commands::EndCommitResponse {},
                        "kiapi.common.commands.EndCommitResponse",
                    ));
                }
                None
            },
        );

        let router = Arc::new(ToolRouter::new());
        router.load("sch_export").await.expect("registered toolset");
        let tool = router
            .get_tool("update_pcb_from_schematic")
            .await
            .expect("registered sync tool");
        let context = Arc::new(ToolContext::new(
            ServerConfig {
                kicad_cli: cli.to_string_lossy().to_string(),
                kicad_binary: String::new(),
                ipc_address: server.address().to_string(),
                project_dir: None,
                jlcpcb_db_path: None,
                auto_load_toolsets: false,
                eager_toolsets: false,
            },
            router,
        ));
        let paths = serde_json::json!({
            "schematic": schematic.to_string_lossy(),
            "board": board.to_string_lossy(),
        });
        let dry_run = (tool.handler)(&paths, context.clone()).await.unwrap();
        let dry_run: serde_json::Value = serde_json::from_str(&match &dry_run.content[0] {
            ToolContent::Text { text } => text.clone(),
            _ => panic!("sync response was not JSON text"),
        })
        .unwrap();
        assert_eq!(dry_run["status"], "ready", "{dry_run:#}");
        let apply = serde_json::json!({
            "schematic": schematic.to_string_lossy(),
            "board": board.to_string_lossy(),
            "dry_run": false,
            "expected_plan_revision": dry_run["plan_revision"],
        });
        let applied = (tool.handler)(&apply, context).await.unwrap();
        let applied: serde_json::Value = serde_json::from_str(&match &applied.content[0] {
            ToolContent::Text { text } => text.clone(),
            _ => panic!("sync response was not JSON text"),
        })
        .unwrap();
        assert_eq!(applied["status"], "applied");
        assert_eq!(applied["coverage"]["conflicts"]["planned"], 0);
        assert_eq!(applied["coverage"]["conflicts"]["applied"], 0);
        assert_eq!(applied["coverage"]["board_only_preserved"]["applied"], 2);

        let readback = copperpour_ipc::KiCadIpcClient::new(server.address().to_string());
        let document = readback
            .find_open_board(&board)
            .expect("the mock still holds the requested board");
        let footprints_after = readback
            .get_items_in(
                document.clone(),
                kiapi::common::types::KiCadObjectType::KotPcbFootprint,
            )
            .expect("footprint readback");
        let zones_after = readback
            .get_items_in(document, kiapi::common::types::KiCadObjectType::KotPcbZone)
            .expect("zone and rule-area readback");
        let before =
            kiapi::board::types::FootprintInstance::decode(footprints_before[0].value.as_slice())
                .expect("captured R1 before apply");
        let after =
            kiapi::board::types::FootprintInstance::decode(footprints_after[0].value.as_slice())
                .expect("captured R1 after apply");
        let before = board_footprint_from_instance(&before).expect("R1 identity before apply");
        let after = board_footprint_from_instance(&after).expect("R1 identity after apply");
        assert_eq!(before.value, "1k");
        assert_eq!(after.value, "10k");
        assert_eq!(after.footprint_id, before.footprint_id);
        assert_eq!(
            after.kiid, before.kiid,
            "the updated footprint keeps its KIID"
        );
        assert_eq!(
            after.position, before.position,
            "the updated footprint keeps its placement"
        );
        assert_eq!(
            &footprints_after[1..],
            &footprints_before[1..],
            "both board-only footprints must retain their complete protobuf identity and geometry"
        );
        assert_eq!(
            zones_after, zones_before,
            "the copper zone and keep-out/rule area must retain their complete protobuf identity and geometry"
        );
    }

    // ─── #657: name what cannot be prepared, and let `ready` mean ready ──────

    const TEXAS_VQFN: &str =
        "Package_DFN_QFN:Texas_RJE0020A_VQFN-20-1EP_3x3mm_P0.45mm_EP0.675x0.76mm";
    const GENERIC_VQFN: &str = "Package_DFN_QFN:VQFN-20-1EP_3x3mm_P0.45mm_EP1.55x1.55mm";
    const STOCK_0603: &str = "Capacitor_SMD:C_0603_1608Metric";
    /// KiCad 10.0.8's stock DFN-14 whose eight thermal-via pads carry
    /// `(property pad_prop_heatsink)` (`tests/fixtures/issue_283_ipc.README.md`).
    const THERMAL_DFN: &str =
        "Package_DFN_QFN:DFN-14-1EP_3x4.5mm_P0.65mm_EP1.65x4.25mm_ThermalVias";

    /// The stock generic VQFN-20 unchanged, in a library of its own: its
    /// custom pads (circle anchors, outline clearance) are placed (#679).
    const PLACEABLE_VQFN: &str = "Custom_Stock:VQFN-20-1EP_3x3mm_P0.45mm_EP1.55x1.55mm";

    /// A project whose own `fp-lib-table` resolves stock KiCad 10 footprints:
    /// the two with custom-shape pads that #657 met on a real board
    /// (provenance in `tests/fixtures/custom_pads_kicad10.README.md`), a plain
    /// 0603, the DFN-14 with heatsink pads, and the generic VQFN once more as
    /// [`PLACEABLE_VQFN`]. Since custom pads are placed (#679), the two #657
    /// footprints are written with `(clearance convexhull)`, which KiCad's pad
    /// message cannot carry, so they stay unusable for a reason that holds. A
    /// project table shadows the global one, so these files are the ones read
    /// whether or not KiCad is installed.
    fn project_with_stock_footprints() -> (tempfile::TempDir, PathBuf) {
        let temp = tempfile::tempdir().unwrap();
        let board = temp.path().join("carrier.kicad_pcb");
        std::fs::write(
            &board,
            include_bytes!("../../tests/fixtures/specctra_two_resistors.kicad_pcb"),
        )
        .unwrap();
        let stock: [(&str, &[u8]); 4] = [
            (
                TEXAS_VQFN,
                include_bytes!("../../tests/fixtures/custom_pads_texas_rje0020a_kicad10.kicad_mod"),
            ),
            (
                GENERIC_VQFN,
                include_bytes!("../../tests/fixtures/custom_pads_vqfn20_kicad10.kicad_mod"),
            ),
            (
                STOCK_0603,
                include_bytes!("../../tests/fixtures/c_0603_1608metric_kicad10.kicad_mod"),
            ),
            (
                THERMAL_DFN,
                include_bytes!(
                    "../../tests/fixtures/issue_283_dfn14_thermalvias_kicad10.kicad_mod"
                ),
            ),
        ];
        let convexhull = |source: &[u8]| {
            let text = std::str::from_utf8(source).unwrap();
            assert!(text.contains("(clearance outline)"));
            text.replace("(clearance outline)", "(clearance convexhull)")
                .into_bytes()
        };
        let mut stock: Vec<(&str, Vec<u8>)> = stock
            .into_iter()
            .map(|(footprint_id, source)| {
                if footprint_id == TEXAS_VQFN || footprint_id == GENERIC_VQFN {
                    (footprint_id, convexhull(source))
                } else {
                    (footprint_id, source.to_vec())
                }
            })
            .collect();
        stock.push((
            PLACEABLE_VQFN,
            include_bytes!("../../tests/fixtures/custom_pads_vqfn20_kicad10.kicad_mod").to_vec(),
        ));
        for (footprint_id, source) in stock {
            let (library, name) = footprint_id.split_once(':').unwrap();
            let directory = temp.path().join(format!("{library}.pretty"));
            std::fs::create_dir_all(&directory).unwrap();
            std::fs::write(directory.join(format!("{name}.kicad_mod")), source).unwrap();
        }
        std::fs::write(
            temp.path().join("fp-lib-table"),
            "(fp_lib_table\n  (lib (name \"Package_DFN_QFN\") (type \"KiCad\") (uri \"${KIPRJMOD}/Package_DFN_QFN.pretty\") (options \"\") (descr \"\"))\n  (lib (name \"Capacitor_SMD\") (type \"KiCad\") (uri \"${KIPRJMOD}/Capacitor_SMD.pretty\") (options \"\") (descr \"\"))\n  (lib (name \"Custom_Stock\") (type \"KiCad\") (uri \"${KIPRJMOD}/Custom_Stock.pretty\") (options \"\") (descr \"\"))\n)\n",
        )
        .unwrap();
        (temp, board)
    }

    fn planned_add(reference: &str, footprint_id: &str, pads: &[&str]) -> PlannedChange {
        PlannedChange::Add {
            reference: reference.to_string(),
            value: "part".to_string(),
            footprint_id: footprint_id.to_string(),
            symbol_path: format!("/{reference}-uuid"),
            dnp: false,
            exclude_from_bom: false,
            pad_nets: pads
                .iter()
                .map(|pad| (pad.to_string(), format!("{reference}-{pad}")))
                .collect(),
            unconnected_pins: BTreeSet::new(),
            position: Point { x: 0.0, y: 0.0 },
            fields: Vec::new(),
            sheet: SymbolSheet::default(),
        }
    }

    fn plan_adding(changes: Vec<PlannedChange>) -> SyncPlan {
        SyncPlan {
            status: PlanStatus::Ready,
            plan_revision: "reviewed".to_string(),
            counts: SyncCounts {
                added: CountPair {
                    planned: changes.len(),
                    applied: 0,
                },
                ..SyncCounts::default()
            },
            changes,
            diagnostics: Vec::new(),
            unassigned: Vec::new(),
            skipped: Vec::new(),
            refused_updates: Vec::new(),
            without_symbols: Vec::new(),
            copper_net_changes: Vec::new(),
            staging: None,
            acknowledged_apply: None,
        }
    }

    /// #657: the first unusable footprint stopped preparation, so a plan that
    /// needed two of them heard about one, and about neither by name.
    #[test]
    fn every_unusable_footprint_is_named_with_the_parts_that_need_it() {
        let (_temp, board) = project_with_stock_footprints();
        let plan = plan_adding(vec![
            planned_add("U1", TEXAS_VQFN, &["1"]),
            planned_add("C1", STOCK_0603, &["1", "2"]),
            planned_add("U3", GENERIC_VQFN, &["1"]),
            planned_add("U2", TEXAS_VQFN, &["1"]),
        ]);

        let (prepared, unprepared) = prepare_additions(&board, &plan);

        assert_eq!(
            prepared.keys().map(String::as_str).collect::<Vec<_>>(),
            [STOCK_0603],
            "a footprint that can be placed is still prepared"
        );
        let diagnostics = unprepared
            .into_iter()
            .map(UnpreparedFootprint::into_diagnostic)
            .collect::<Vec<_>>();
        assert_eq!(diagnostics.len(), 2, "{diagnostics:#?}");

        let texas = &diagnostics[0];
        assert_eq!(texas.code, "unsupported_library_footprint");
        assert_eq!(texas.footprint_id.as_deref(), Some(TEXAS_VQFN));
        assert_eq!(texas.references, ["U1", "U2"]);
        assert_eq!(
            texas.reference, None,
            "two parts are concerned, so no single one is named"
        );
        assert!(
            texas.message.contains(TEXAS_VQFN)
                && texas.message.contains("U1, U2")
                && texas.message.contains("custom pad '1'")
                && texas.message.contains("(clearance convexhull)"),
            "{}",
            texas.message
        );

        let generic = &diagnostics[1];
        assert_eq!(generic.code, "unsupported_library_footprint");
        assert_eq!(generic.footprint_id.as_deref(), Some(GENERIC_VQFN));
        assert_eq!(generic.references, ["U3"]);
        assert_eq!(generic.reference.as_deref(), Some("U3"));
    }

    /// A stock footprint with custom pads is prepared for Update PCB's
    /// additions (#679): each custom pad carries its anchor and primitives.
    #[test]
    fn a_stock_footprint_with_custom_pads_is_prepared() {
        let (_temp, board) = project_with_stock_footprints();
        let plan = plan_adding(vec![planned_add("U9", PLACEABLE_VQFN, &["1"])]);

        let (prepared, unprepared) = prepare_additions(&board, &plan);

        assert!(unprepared.is_empty(), "{unprepared:?}");
        let part = prepared.get(PLACEABLE_VQFN).expect("the VQFN is prepared");
        let custom = part
            .pads
            .iter()
            .filter_map(|pad| {
                pad.custom
                    .as_ref()
                    .map(|custom| (pad.number.as_str(), custom))
            })
            .collect::<Vec<_>>();
        assert_eq!(
            custom.iter().map(|(number, _)| *number).collect::<Vec<_>>(),
            ["1", "5", "6", "10", "11", "15", "16", "20"]
        );
        let (_, first) = custom[0];
        assert_eq!(first.anchor, copperpour_ipc::IpcCustomPadAnchor::Circle);
        assert_eq!(
            first.primitives,
            [copperpour_ipc::IpcPadPrimitive::Polygon {
                points: vec![
                    (-0.375, -0.075),
                    (0.254289, -0.075),
                    (0.375, 0.045711),
                    (0.375, 0.075),
                    (-0.375, 0.075)
                ],
                width: 0.1,
                filled: true
            }]
        );
    }

    /// The caller's next step differs with the stage that failed: fix the
    /// library table, fix the file, or substitute the footprint. The codes are
    /// the ones `update_footprints_from_library` reports for the same three.
    #[test]
    fn each_stage_of_preparation_fails_under_its_own_code() {
        let (temp, board) = project_with_stock_footprints();
        std::fs::write(
            temp.path()
                .join("Capacitor_SMD.pretty/Unreadable.kicad_mod"),
            [0xff, 0xfe, 0x00, 0x28],
        )
        .unwrap();
        let plan = plan_adding(vec![
            planned_add("J1", "Konnect_No_Such_Library:Nothing", &["1"]),
            planned_add("C9", "Capacitor_SMD:Unreadable", &["1"]),
            planned_add("U1", TEXAS_VQFN, &["1"]),
        ]);

        let (prepared, unprepared) = prepare_additions(&board, &plan);

        assert!(prepared.is_empty());
        let by_reference = unprepared
            .iter()
            .map(|failed| (failed.references[0].as_str(), failed.code))
            .collect::<BTreeMap<_, _>>();
        assert_eq!(
            by_reference,
            BTreeMap::from([
                ("C9", "footprint_library_read_failed"),
                ("J1", "footprint_library_resolution_failed"),
                ("U1", "unsupported_library_footprint"),
            ])
        );
    }

    /// A message names at most eight parts; `references` names them all.
    #[test]
    fn a_message_is_bounded_and_the_reference_list_is_complete() {
        let references = (1..=30).map(|n| format!("C{n}")).collect::<Vec<_>>();
        let diagnostic = UnpreparedFootprint {
            footprint_id: TEXAS_VQFN.to_string(),
            references: references.clone(),
            code: "unsupported_library_footprint",
            reason: "custom-shape pads".to_string(),
        }
        .into_diagnostic();

        assert_eq!(diagnostic.references, references);
        assert!(
            diagnostic.message.contains("C8 and 22 more") && !diagnostic.message.contains("C9,"),
            "{}",
            diagnostic.message
        );
    }

    /// Add path: `footprint C1 has no pad 3` used to be raised only inside
    /// the apply, after a dry run that said `ready`.
    #[test]
    fn a_connected_pad_the_library_footprint_lacks_is_found_while_planning() {
        let (_temp, board) = project_with_stock_footprints();
        let plan = plan_adding(vec![
            planned_add("C1", STOCK_0603, &["1", "2", "3", "4"]),
            planned_add("C2", STOCK_0603, &["1", "2"]),
        ]);
        let (prepared, unprepared) = prepare_additions(&board, &plan);
        assert!(unprepared.is_empty());

        let diagnostics = additions_missing_pads(&plan, &prepared);

        assert_eq!(diagnostics.len(), 1, "{diagnostics:#?}");
        let missing = &diagnostics[0];
        assert_eq!(missing.code, "footprint_pad_missing");
        assert_eq!(missing.reference.as_deref(), Some("C1"));
        assert_eq!(missing.references, ["C1"]);
        assert_eq!(missing.footprint_id.as_deref(), Some(STOCK_0603));
        assert!(missing.message.contains("pads 3, 4"), "{}", missing.message);
    }

    /// Update path: the same rule against the pads of the live footprint.
    /// Before #657, this planned an update with one pad reassigned and said
    /// `ready`; the apply then failed on `footprint R1 has no pad 3`. Then it
    /// refused the whole plan. Now only that update is refused, for the
    /// handler to skip the part (`refused_updates`), as it skips an added
    /// part that lacks a pad.
    #[test]
    fn a_connected_pad_the_live_footprint_lacks_is_found_while_planning() {
        let mut component = resistor("R1", "/sheet/existing");
        component
            .pad_nets
            .insert("3".to_string(), "SENSE".to_string());
        let design = ExportedDesign {
            root_sheet_uuid: None,
            components: vec![component],
            skipped: Vec::new(),
            unassigned: Vec::new(),
            net_names: BTreeSet::new(),
        };
        let board = board_with(vec![board_resistor("R1", Some("/sheet/existing"))]);

        let plan = plan_sync("netlist", &design, &board);

        assert_eq!(plan.status, PlanStatus::Noop);
        assert!(plan.changes.is_empty());
        assert_eq!(plan.counts.updated.planned, 0);
        assert_eq!(plan.diagnostics, Vec::new());
        assert_eq!(plan.refused_updates.len(), 1, "{:#?}", plan.refused_updates);
        let missing = &plan.refused_updates[0];
        assert_eq!(missing.code, "footprint_pad_missing");
        assert_eq!(missing.reference.as_deref(), Some("R1"));
        assert_eq!(
            missing.footprint_id.as_deref(),
            Some("Resistor_SMD:R_0603_1608Metric")
        );
        assert!(missing.message.contains("pad 3"), "{}", missing.message);
    }

    /// A keyed header's footprint has no pad at the key, and the schematic's
    /// pin there has a no-connect flag. That pin connects nothing, so the
    /// footprint is updated, and its pad nets name only the pads it has: the
    /// apply sets each of them (`apply_footprint_fields` refuses any other).
    /// With nothing else to change, the plan is a no-op, not an update of a
    /// pad that does not exist.
    #[test]
    fn a_live_footprint_needs_no_pad_for_a_pin_kicad_left_unconnected() {
        let keyed = |value: &str| {
            let mut component = resistor("R1", "/sheet/existing");
            component.value = value.to_string();
            component
                .pad_nets
                .insert("3".to_string(), "unconnected-(R1-Pad3)".to_string());
            component.unconnected_pins.insert("3".to_string());
            ExportedDesign {
                root_sheet_uuid: None,
                components: vec![component],
                skipped: Vec::new(),
                unassigned: Vec::new(),
                net_names: BTreeSet::new(),
            }
        };
        let board = board_with(vec![board_resistor("R1", Some("/sheet/existing"))]);

        let unchanged = plan_sync("netlist", &keyed("10k"), &board);
        assert_eq!(unchanged.status, PlanStatus::Noop, "{unchanged:#?}");
        assert_eq!(unchanged.refused_updates, Vec::new());

        let plan = plan_sync("netlist", &keyed("22k"), &board);
        assert_eq!(plan.status, PlanStatus::Ready, "{plan:#?}");
        assert_eq!(plan.diagnostics, Vec::new());
        assert_eq!(plan.refused_updates, Vec::new());
        assert_eq!(plan.counts.pads_reassigned.planned, 0);
        let [PlannedChange::Update {
            value, pad_nets, ..
        }] = plan.changes.as_slice()
        else {
            panic!("one update: {:#?}", plan.changes);
        };
        assert_eq!(value, "22k");
        assert_eq!(pad_nets, &board.footprints[0].pad_nets);
    }

    /// The netlist lists one UUID per unit of a multi-unit symbol, and a
    /// footprint may be linked to any of them: KiCad's Update PCB matches it
    /// by each. One linked to the second unit used to be taken for another
    /// symbol's (`reference_identity_conflict`), which refused the plan.
    #[test]
    fn a_footprint_linked_to_a_later_unit_of_its_symbol_is_matched() {
        let mut component = resistor("U1", "/sheet/unit-a");
        component.other_unit_paths = vec!["/sheet/unit-b".to_string(), "/sheet/unit-e".to_string()];
        component.value = "LM339".to_string();
        let design = ExportedDesign {
            root_sheet_uuid: None,
            components: vec![component],
            skipped: Vec::new(),
            unassigned: Vec::new(),
            net_names: BTreeSet::new(),
        };
        let board = board_with(vec![board_resistor("U1", Some("/sheet/unit-b"))]);

        let plan = plan_sync("netlist", &design, &board);

        assert_eq!(plan.status, PlanStatus::Ready, "{plan:#?}");
        assert_eq!(plan.diagnostics, Vec::new());
        assert_eq!(plan.without_symbols, Vec::new());
        let [PlannedChange::Update {
            kiid,
            value,
            symbol_path,
            ..
        }] = plan.changes.as_slice()
        else {
            panic!("one update: {:#?}", plan.changes);
        };
        assert_eq!(kiid, "U1-kiid");
        assert_eq!(value, "LM339");
        // Its link is kept, as every matched footprint's is.
        assert_eq!(symbol_path, "/sheet/unit-b");
    }

    /// Two footprints, each linked to a unit of one symbol, are not one
    /// symbol's footprint: neither is picked, and the plan is refused with
    /// both named.
    #[test]
    fn footprints_linked_to_two_units_of_one_symbol_are_a_conflict() {
        let mut component = resistor("U1", "/sheet/unit-a");
        component.other_unit_paths = vec!["/sheet/unit-b".to_string()];
        let design = ExportedDesign {
            root_sheet_uuid: None,
            components: vec![component],
            skipped: Vec::new(),
            unassigned: Vec::new(),
            net_names: BTreeSet::new(),
        };
        let mut second = board_resistor("U2", Some("/sheet/unit-b"));
        second.kiid = "U2-kiid".to_string();
        let board = board_with(vec![board_resistor("U1", Some("/sheet/unit-a")), second]);

        let plan = plan_sync("netlist", &design, &board);

        assert_eq!(plan.status, PlanStatus::Conflict);
        assert_eq!(plan.changes, Vec::new());
        assert_eq!(plan.diagnostics.len(), 1, "{:#?}", plan.diagnostics);
        assert_eq!(plan.diagnostics[0].code, "duplicate_board_identity");
        assert_eq!(
            plan.diagnostics[0].message,
            "board footprints U1, U2 are each linked to a unit of U1"
        );
        // Neither is reported as a footprint whose symbol was removed.
        assert_eq!(plan.without_symbols, Vec::new());
    }

    /// The pad set is every pad KiCad sent, netted or not. The 0402 from the
    /// #474 capture has pads 1 and 2. The captured mounting hole has one pad,
    /// with the empty number and no net: exactly the pad `pad_nets` never
    /// held, which is why it cannot answer whether a pad exists.
    #[test]
    fn the_live_footprint_records_every_pad_kicad_sent() {
        use copperpour_ipc::gen::kiapi;
        const RESISTOR: &[u8] = include_bytes!("../../tests/fixtures/issue_474_r1.ipc.bin");
        let resistor = kiapi::board::types::FootprintInstance::decode(RESISTOR)
            .expect("the checked-in KiCad IPC capture must decode");
        let mounting_hole = kiapi::board::types::FootprintInstance::decode(BOARD_ONLY_CAPTURE)
            .expect("the checked-in KiCad IPC capture must decode");

        let resistor = board_footprint_from_instance(&resistor).unwrap();
        let mounting_hole = board_footprint_from_instance(&mounting_hole).unwrap();

        assert_eq!(
            resistor.pad_numbers,
            BTreeSet::from(["1".to_string(), "2".to_string()])
        );
        assert_eq!(mounting_hole.pad_numbers, BTreeSet::from([String::new()]));
        assert!(mounting_hole.pad_nets.is_empty());
    }

    /// A single-part diagnostic lists that part in `references` too, so a
    /// caller can read one field for every diagnostic.
    #[test]
    fn a_single_part_diagnostic_lists_its_part() {
        let diagnostic = conflict("duplicate_board_reference", "message".into(), Some("R1"));
        assert_eq!(diagnostic.reference.as_deref(), Some("R1"));
        assert_eq!(diagnostic.references, ["R1"]);
        assert_eq!(diagnostic.footprint_id, None);

        let board_level = conflict("stale_plan_revision", "message".into(), None);
        assert!(board_level.references.is_empty());
    }

    /// One `(comp …)` per part and one net per pin, in the shape
    /// `kicad-cli sch export netlist --format kicadsexpr` writes.
    fn exported_netlist(components: &[(&str, &str, &[&str])]) -> String {
        let mut parts = String::new();
        let mut nets = String::new();
        let mut code = 0;
        for (reference, footprint_id, pins) in components {
            let pin_list = pins
                .iter()
                .map(|pin| format!("(pin (num \"{pin}\"))"))
                .collect::<String>();
            parts.push_str(&format!(
                "    (comp\n      (ref \"{reference}\")\n      (value \"part\")\n      (footprint \"{footprint_id}\")\n      (sheetpath (names \"/\") (tstamps \"/\"))\n      (tstamps \"{reference}-uuid\")\n      (units (unit (name \"A\") (pins {pin_list}))))\n"
            ));
            for pin in *pins {
                code += 1;
                nets.push_str(&format!(
                    "    (net (code \"{code}\") (name \"{reference}-{pin}\") (class \"Default\")\n      (node (ref \"{reference}\") (pin \"{pin}\") (pintype \"passive\")))\n"
                ));
            }
        }
        format!("(export\n  (components\n{parts}  )\n  (nets\n{nets}  ))\n")
    }

    /// Everything a served dry run needs without KiCad: a stand-in
    /// `kicad-cli` that hands back `netlist`, and a protobuf mock holding the
    /// project's board open with nothing on it. It can also apply: the mock
    /// keeps what `CreateItems` sent and lists it back as the board's
    /// footprints, shaped by `readback`, and `UpdateItems` replaces the
    /// footprint with the KIID sent, as KiCad does.
    struct ServedSync {
        _temp: tempfile::TempDir,
        _kicad: crate::test_support::MockIpcServer,
        handler: crate::mcp::handler::McpHandler,
        schematic: PathBuf,
        board: PathBuf,
        exported: PathBuf,
        /// The report the stand-in `kicad-cli pcb drc` writes; it fails while
        /// no test has written one.
        drc_report: PathBuf,
        /// The zones the mock lists once a commit has ended, when set.
        zones_after_commit: std::sync::Arc<std::sync::Mutex<Option<Vec<prost_types::Any>>>>,
        created: std::sync::Arc<std::sync::Mutex<Vec<prost_types::Any>>>,
        /// The held footprints as the mock's board now has them.
        held: std::sync::Arc<std::sync::Mutex<Vec<prost_types::Any>>>,
        /// What `SaveDocumentToString` answers with.
        board_text: std::sync::Arc<std::sync::Mutex<String>>,
        /// The command each request carried, in order.
        commands: std::sync::Arc<std::sync::Mutex<Vec<String>>>,
        /// What `GetVersion` answers: `(major, minor, patch)`.
        kicad_version: std::sync::Arc<std::sync::Mutex<(u32, u32, u32)>>,
        /// When set, `DeleteItems` answers OK and deletes nothing.
        keeps_deleted: std::sync::Arc<std::sync::atomic::AtomicBool>,
        /// When set, `DeleteItems` reports each requested item, as KiCad
        /// 10.0.7+ does (KiCad `77e2d53da2`); unset, it answers as KiCad
        /// before 10.0.7, with no per-item results.
        reports_deleted: std::sync::Arc<std::sync::atomic::AtomicBool>,
        /// Listing the footprints after a commit fails from this listing on,
        /// counted from 0; `usize::MAX` (the default) never fails.
        fails_readback: std::sync::Arc<std::sync::atomic::AtomicUsize>,
        /// The vias the mock's board holds, as `GetItems` lists them (#862).
        vias: std::sync::Arc<std::sync::Mutex<Vec<prost_types::Any>>>,
        /// The zones the mock's board holds, as `UpdateItems` left them.
        zones: std::sync::Arc<std::sync::Mutex<Vec<prost_types::Any>>>,
        /// What `GetConnectedItems` answers, by the KIID asked about.
        connected: std::sync::Arc<std::sync::Mutex<BTreeMap<String, Vec<prost_types::Any>>>>,
        /// When set, `UpdateItems` answers OK and leaves vias and zones as
        /// they were.
        keeps_copper: std::sync::Arc<std::sync::atomic::AtomicBool>,
        /// When set, listing the vias or zones fails once a commit has ended.
        fails_copper_readback: std::sync::Arc<std::sync::atomic::AtomicBool>,
    }

    /// A listed via's or zone's KIID, decoded as its own type (#862).
    fn copper_kiid(item: &prost_types::Any) -> String {
        use copperpour_ipc::gen::kiapi::board::types::{Via, Zone};
        let id = if copperpour_ipc::builders::any_is(item, "kiapi.board.types.Via") {
            Via::decode(item.value.as_slice())
                .ok()
                .and_then(|via| via.id)
        } else {
            Zone::decode(item.value.as_slice())
                .ok()
                .and_then(|zone| zone.id)
        };
        id.map(|id| id.value).unwrap_or_default()
    }

    /// A listed footprint's KIID.
    fn footprint_kiid(item: &prost_types::Any) -> String {
        use prost::Message;
        copperpour_ipc::gen::kiapi::board::types::FootprintInstance::decode(item.value.as_slice())
            .ok()
            .and_then(|footprint| footprint.id)
            .map(|id| id.value)
            .unwrap_or_default()
    }

    /// A held board table in `ServedSync`: its KIID and bounding box `(x, y, width, height)` in mm.
    type HeldTable = (&'static str, (f64, f64, f64, f64));

    /// What the mock's board holds before the sync touches it.
    #[derive(Clone, Default)]
    struct HeldBoard {
        footprints: Vec<prost_types::Any>,
        zones: Vec<prost_types::Any>,
        nets: Vec<&'static str>,
    }

    /// What the mock's board holds after `CreateItems`.
    #[derive(Clone, Copy)]
    enum Readback {
        /// Refuse creation before there is any new footprint to read back.
        RefuseCreation,
        /// Exactly what was sent, as a KiCad that kept everything does.
        AsSent,
        /// What was sent without the instance attributes and 3D models: the
        /// board the sync built before #789.
        WithoutLibraryData,
        /// What was sent with its mounting style and models, but DNP cleared:
        /// one flag lost while everything the old check looked at survives.
        WithoutDnp,
        /// What was sent without its custom fields, and with Value moved back
        /// to the hidden silkscreen field the builder used to give it.
        WithoutLibraryFields,
        /// What was sent with every pad's fabrication property cleared, as
        /// KiCad before 10.0.7 would read it.
        WithoutPadProperties,
        /// What was sent, with each pad sent without a fabrication property
        /// reported as `PFP_NONE`, as KiCad 10.0.7+ reports it.
        UnknownPadPropertiesAsNone,
        /// What was sent without its sheet name, sheet file and footprint
        /// filters: the board the sync built before #237.
        WithoutSheet,
    }

    impl ServedSync {
        async fn new() -> Self {
            Self::holding_outline(None).await
        }

        /// As [`Self::new`], with KiCad also holding one board graphic whose
        /// box is `outline`, `(x, y, width, height)` in mm.
        async fn holding_outline(outline: Option<(f64, f64, f64, f64)>) -> Self {
            Self::build(
                outline,
                Vec::new(),
                Vec::new(),
                Readback::AsSent,
                HeldBoard::default(),
            )
            .await
        }

        /// As [`Self::holding_outline`], with KiCad refusing to list each class
        /// in `refused`, as KiCad 10.0.5 refuses tables and generators.
        async fn refusing(
            outline: Option<(f64, f64, f64, f64)>,
            refused: Vec<copperpour_ipc::gen::kiapi::common::types::KiCadObjectType>,
        ) -> Self {
            Self::build(
                outline,
                Vec::new(),
                refused,
                Readback::AsSent,
                HeldBoard::default(),
            )
            .await
        }

        /// As [`Self::new`], with the board read back as `readback` says.
        async fn reading_back(readback: Readback) -> Self {
            Self::build(None, Vec::new(), Vec::new(), readback, HeldBoard::default()).await
        }

        /// As [`Self::new`], with KiCad already holding `held`.
        async fn holding(held: HeldBoard) -> Self {
            Self::build(None, Vec::new(), Vec::new(), Readback::AsSent, held).await
        }

        async fn build(
            outline: Option<(f64, f64, f64, f64)>,
            held_tables: Vec<HeldTable>,
            refused: Vec<copperpour_ipc::gen::kiapi::common::types::KiCadObjectType>,
            readback: Readback,
            held: HeldBoard,
        ) -> Self {
            use crate::tools::cli::test_support::write_script;
            use copperpour_ipc::gen::kiapi;

            let created =
                std::sync::Arc::new(std::sync::Mutex::new(Vec::<prost_types::Any>::new()));
            let board_items = created.clone();
            let held_kiids = held
                .footprints
                .iter()
                .map(footprint_kiid)
                .chain(held.zones.iter().map(|item| {
                    kiapi::board::types::Zone::decode(item.value.as_slice())
                        .ok()
                        .and_then(|zone| zone.id)
                        .map(|id| id.value)
                        .unwrap_or_default()
                }))
                .collect::<Vec<_>>();
            let held_footprints = std::sync::Arc::new(std::sync::Mutex::new(held.footprints));
            let board_held = held_footprints.clone();
            let zones_held = std::sync::Arc::new(std::sync::Mutex::new(held.zones));
            let held_zones = zones_held.clone();
            let zones_updated = zones_held.clone();
            let vias_held =
                std::sync::Arc::new(std::sync::Mutex::new(Vec::<prost_types::Any>::new()));
            let held_vias = vias_held.clone();
            let vias_updated = vias_held.clone();
            let vias_measured = vias_held.clone();
            let connected = std::sync::Arc::new(std::sync::Mutex::new(BTreeMap::<
                String,
                Vec<prost_types::Any>,
            >::new()));
            let connected_items = connected.clone();
            let keeps_copper = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
            let ignores_copper = keeps_copper.clone();
            let fails_copper_readback =
                std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
            let copper_readback_fails = fails_copper_readback.clone();
            let zones_after_commit =
                std::sync::Arc::new(std::sync::Mutex::new(None::<Vec<prost_types::Any>>));
            let after_commit = zones_after_commit.clone();
            let committed = std::sync::atomic::AtomicBool::new(false);
            let held_nets = held.nets;
            let board_text = std::sync::Arc::new(std::sync::Mutex::new(
                String::from_utf8_lossy(include_bytes!(
                    "../../tests/fixtures/specctra_two_resistors.kicad_pcb"
                ))
                .into_owned(),
            ));
            let text = board_text.clone();
            let commands = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
            let seen = commands.clone();
            let kicad_version = std::sync::Arc::new(std::sync::Mutex::new((10, 0, 8)));
            let version = kicad_version.clone();
            let keeps_deleted = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
            let ignores_deletes = keeps_deleted.clone();
            let reports_deleted = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
            let reports_each_deletion = reports_deleted.clone();
            let fails_readback =
                std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(usize::MAX));
            let readback_fails = fails_readback.clone();
            let listed_after_commit = std::sync::atomic::AtomicUsize::new(0);

            let (temp, board) = project_with_stock_footprints();
            let schematic = temp.path().join("carrier.kicad_sch");
            std::fs::write(
                &schematic,
                include_bytes!("../../tests/fixtures/structural_scans_kicad10.kicad_sch"),
            )
            .unwrap();
            let exported = temp.path().join("carrier.net");
            let unix_source = exported.to_string_lossy().replace('\'', "'\\''");
            let windows_source = exported.to_string_lossy();
            let drc_report = temp.path().join("drc-report.json");
            let unix_drc = drc_report.to_string_lossy().replace('\'', "'\\''");
            let windows_drc = drc_report.to_string_lossy();
            let cli = write_script(
                temp.path(),
                "fake-kicad-cli-657",
                &format!(
                    "#!/bin/sh\nsource='{unix_source}'\nwhile [ \"$#\" -gt 0 ]; do\n  if [ \"$1\" = \"drc\" ]; then\n    source='{unix_drc}'\n  fi\n  if [ \"$1\" = \"--output\" ]; then\n    shift\n    cp \"$source\" \"$1\"\n    exit $?\n  fi\n  shift\ndone\nexit 2\n"
                ),
                &format!(
                    "@echo off\r\nset \"source={windows_source}\"\r\n:loop\r\nif \"%~1\"==\"\" exit /b 2\r\nif \"%~1\"==\"drc\" set \"source={windows_drc}\"\r\nif \"%~1\"==\"--output\" goto found\r\nshift\r\ngoto loop\r\n:found\r\nshift\r\ncopy /Y \"%source%\" \"%~1\" >nul\r\nexit /b %ERRORLEVEL%\r\n"
                ),
            );
            let kicad = crate::tools::pcb_board::board_mock::spawn_kicad_holding_board_refusing(
                &board,
                refused,
                move |command| {
                    let name = command.type_url.rsplit('.').next().unwrap_or_default();
                    seen.lock().unwrap().push(name.to_string());
                    if command.type_url.ends_with("GetVersion") {
                        let (major, minor, patch) = *version.lock().unwrap();
                        return Some(copperpour_ipc::builders::pack_any(
                            &kiapi::common::commands::GetVersionResponse {
                                version: Some(kiapi::common::types::KiCadVersion {
                                    major,
                                    minor,
                                    patch,
                                    full_version: format!("{major}.{minor}.{patch}"),
                                }),
                            },
                            "kiapi.common.commands.GetVersionResponse",
                        ));
                    }
                    if command.type_url.ends_with("GetItems") {
                        let request =
                            kiapi::common::commands::GetItems::decode(command.value.as_slice())
                                .expect("GetItems request");
                        let shapes = kiapi::common::types::KiCadObjectType::KotPcbShape as i32;
                        let footprints =
                            kiapi::common::types::KiCadObjectType::KotPcbFootprint as i32;
                        let zones = kiapi::common::types::KiCadObjectType::KotPcbZone as i32;
                        let vias = kiapi::common::types::KiCadObjectType::KotPcbVia as i32;
                        let tables = kiapi::common::types::KiCadObjectType::KotPcbTable as i32;
                        let items = match outline {
                            Some(_) if request.types.contains(&shapes) => {
                                vec![crate::tools::pcb_board::board_mock::listed_item(
                                    kiapi::common::types::KiCadObjectType::KotPcbShape,
                                    "outline",
                                )]
                            }
                            _ if request.types.contains(&footprints)
                                && committed.load(std::sync::atomic::Ordering::SeqCst)
                                && listed_after_commit
                                    .fetch_add(1, std::sync::atomic::Ordering::SeqCst)
                                    >= readback_fails.load(std::sync::atomic::Ordering::SeqCst) =>
                            {
                                return Some(copperpour_ipc::builders::pack_any(
                                    &kiapi::common::commands::GetItemsResponse {
                                        header: None,
                                        status: kiapi::common::types::ItemRequestStatus::IrsDocumentNotFound
                                            as i32,
                                        items: Vec::new(),
                                    },
                                    "kiapi.common.commands.GetItemsResponse",
                                ));
                            }
                            _ if request.types.contains(&footprints) => board_held
                                .lock()
                                .unwrap()
                                .iter()
                                .cloned()
                                .chain(
                                    board_items
                                        .lock()
                                        .unwrap()
                                        .iter()
                                        .map(|item| read_back(item, readback)),
                                )
                                .collect(),
                            _ if (request.types.contains(&zones)
                                || request.types.contains(&vias))
                                && copper_readback_fails
                                    .load(std::sync::atomic::Ordering::SeqCst)
                                && committed.load(std::sync::atomic::Ordering::SeqCst) =>
                            {
                                return Some(copperpour_ipc::builders::pack_any(
                                    &kiapi::common::commands::GetItemsResponse {
                                        header: None,
                                        status: kiapi::common::types::ItemRequestStatus::IrsDocumentNotFound
                                            as i32,
                                        items: Vec::new(),
                                    },
                                    "kiapi.common.commands.GetItemsResponse",
                                ));
                            }
                            _ if request.types.contains(&zones) => {
                                match &*after_commit.lock().unwrap() {
                                    Some(after)
                                        if committed.load(std::sync::atomic::Ordering::SeqCst) =>
                                    {
                                        after.clone()
                                    }
                                    _ => held_zones.lock().unwrap().clone(),
                                }
                            }
                            _ if request.types.contains(&vias) => held_vias.lock().unwrap().clone(),
                            _ if request.types.contains(&tables) => held_tables
                                .iter()
                                .map(|(kiid, _)| {
                                    crate::tools::pcb_board::board_mock::listed_item(
                                        kiapi::common::types::KiCadObjectType::KotPcbTable,
                                        kiid,
                                    )
                                })
                                .collect(),
                            _ => Vec::new(),
                        };
                        return Some(copperpour_ipc::builders::pack_any(
                            &kiapi::common::commands::GetItemsResponse {
                                header: None,
                                status: kiapi::common::types::ItemRequestStatus::IrsOk as i32,
                                items,
                            },
                            "kiapi.common.commands.GetItemsResponse",
                        ));
                    }
                    if command.type_url.ends_with("GetNets") {
                        return Some(copperpour_ipc::builders::pack_any(
                            &kiapi::board::commands::NetsResponse {
                                nets: held_nets
                                    .iter()
                                    .zip(1..)
                                    .map(|(name, code)| kiapi::board::types::Net {
                                        code: Some(kiapi::board::types::NetCode { value: code }),
                                        name: name.to_string(),
                                    })
                                    .collect(),
                            },
                            "kiapi.board.commands.NetsResponse",
                        ));
                    }
                    if command.type_url.ends_with("GetBoardEnabledLayers") {
                        // A two-layer board, as `specctra_two_resistors` is.
                        return Some(copperpour_ipc::builders::pack_any(
                            &kiapi::board::commands::BoardEnabledLayersResponse {
                                copper_layer_count: 2,
                                layers: vec![
                                    kiapi::board::types::BoardLayer::BlFCu as i32,
                                    kiapi::board::types::BoardLayer::BlBCu as i32,
                                    kiapi::board::types::BoardLayer::BlEdgeCuts as i32,
                                ],
                            },
                            "kiapi.board.commands.BoardEnabledLayersResponse",
                        ));
                    }
                    if command.type_url.ends_with("GetBoundingBox") {
                        return Some(crate::tools::pcb_board::board_mock::kicad_bounding_boxes(
                            command,
                            |kiid| {
                                if kiid == "outline" {
                                    return outline.expect("an outline to measure");
                                }
                                if let Some((_, bounds)) =
                                    held_tables.iter().find(|(id, _)| *id == kiid)
                                {
                                    return *bounds;
                                }
                                let held_via = vias_measured
                                    .lock()
                                    .unwrap()
                                    .iter()
                                    .any(|via| copper_kiid(via) == kiid);
                                assert!(
                                    held_via || held_kiids.iter().any(|held| held == kiid),
                                    "only the outline and held items are listed, not {kiid}"
                                );
                                (0.0, 0.0, 10.0, 10.0)
                            },
                        ));
                    }
                    if command.type_url.ends_with("SaveDocumentToString") {
                        // The board as KiCad holds it: the saved file, unless a
                        // test holds another.
                        let request = kiapi::common::commands::SaveDocumentToString::decode(
                            command.value.as_slice(),
                        )
                        .expect("SaveDocumentToString request");
                        return Some(copperpour_ipc::builders::pack_any(
                            &kiapi::common::commands::SavedDocumentResponse {
                                document: request.document,
                                contents: text.lock().unwrap().clone(),
                            },
                            "kiapi.common.commands.SavedDocumentResponse",
                        ));
                    }
                    if command.type_url.ends_with("BeginCommit") {
                        return Some(copperpour_ipc::builders::pack_any(
                            &kiapi::common::commands::BeginCommitResponse {
                                id: Some(kiapi::common::types::Kiid {
                                    value: "served-sync-commit".into(),
                                }),
                            },
                            "kiapi.common.commands.BeginCommitResponse",
                        ));
                    }
                    if command.type_url.ends_with("CreateItems") {
                        use kiapi::common::commands as cmd;
                        let request = cmd::CreateItems::decode(command.value.as_slice())
                            .expect("CreateItems request");
                        let refused = matches!(readback, Readback::RefuseCreation);
                        if !refused {
                            board_items
                                .lock()
                                .unwrap()
                                .extend(request.items.iter().cloned());
                        }
                        return Some(copperpour_ipc::builders::pack_any(
                            &kiapi::common::commands::CreateItemsResponse {
                                header: None,
                                status: kiapi::common::types::ItemRequestStatus::IrsOk as i32,
                                created_items: request
                                    .items
                                    .into_iter()
                                    .map(|item| kiapi::common::commands::ItemCreationResult {
                                        status: Some(kiapi::common::commands::ItemStatus {
                                            code: if refused {
                                                cmd::ItemStatusCode::IscInvalidData
                                            } else {
                                                cmd::ItemStatusCode::IscOk
                                            }
                                                as i32,
                                            error_message: if refused {
                                                "footprint refused".into()
                                            } else {
                                                String::new()
                                            },
                                        }),
                                        item: (!refused).then_some(item),
                                    })
                                    .collect(),
                            },
                            "kiapi.common.commands.CreateItemsResponse",
                        ));
                    }
                    if command.type_url.ends_with("GetConnectedItems") {
                        let request = kiapi::board::commands::GetConnectedItems::decode(
                            command.value.as_slice(),
                        )
                        .expect("GetConnectedItems request");
                        let connected = connected_items.lock().unwrap();
                        let items = request
                            .items
                            .iter()
                            .flat_map(|id| connected.get(&id.value).cloned().unwrap_or_default())
                            .collect();
                        return Some(copperpour_ipc::builders::pack_any(
                            &kiapi::common::commands::GetItemsResponse {
                                header: None,
                                status: kiapi::common::types::ItemRequestStatus::IrsOk as i32,
                                items,
                            },
                            "kiapi.common.commands.GetItemsResponse",
                        ));
                    }
                    if command.type_url.ends_with("UpdateItems") {
                        let request =
                            kiapi::common::commands::UpdateItems::decode(command.value.as_slice())
                                .expect("UpdateItems request");
                        if !ignores_copper.load(std::sync::atomic::Ordering::SeqCst) {
                            for updated in &request.items {
                                let copper = if copperpour_ipc::builders::any_is(
                                    updated,
                                    "kiapi.board.types.Zone",
                                ) {
                                    &zones_updated
                                } else if copperpour_ipc::builders::any_is(
                                    updated,
                                    "kiapi.board.types.Via",
                                ) {
                                    &vias_updated
                                } else {
                                    continue;
                                };
                                for item in copper.lock().unwrap().iter_mut() {
                                    if copper_kiid(item) == copper_kiid(updated) {
                                        *item = updated.clone();
                                    }
                                }
                            }
                        }
                        let mut held = board_held.lock().unwrap();
                        for updated in &request.items {
                            for footprint in held.iter_mut() {
                                if footprint_kiid(footprint) == footprint_kiid(updated) {
                                    *footprint = updated.clone();
                                }
                            }
                        }
                        drop(held);
                        return Some(copperpour_ipc::builders::pack_any(
                            &kiapi::common::commands::UpdateItemsResponse {
                                header: None,
                                status: kiapi::common::types::ItemRequestStatus::IrsOk as i32,
                                updated_items: request
                                    .items
                                    .into_iter()
                                    .map(|item| kiapi::common::commands::ItemUpdateResult {
                                        status: Some(kiapi::common::commands::ItemStatus {
                                            code: kiapi::common::commands::ItemStatusCode::IscOk
                                                as i32,
                                            error_message: String::new(),
                                        }),
                                        item: Some(item),
                                    })
                                    .collect(),
                            },
                            "kiapi.common.commands.UpdateItemsResponse",
                        ));
                    }
                    if command.type_url.ends_with("DeleteItems") {
                        // OK, with no per-item results as KiCad before 10.0.7
                        // answers, or one per item as 10.0.7+ does.
                        use kiapi::common::commands::{ItemDeletionResult, ItemDeletionStatus};
                        let request =
                            kiapi::common::commands::DeleteItems::decode(command.value.as_slice())
                                .expect("DeleteItems request");
                        let held = board_held
                            .lock()
                            .unwrap()
                            .iter()
                            .map(footprint_kiid)
                            .collect::<BTreeSet<_>>();
                        if !ignores_deletes.load(std::sync::atomic::Ordering::SeqCst) {
                            board_held.lock().unwrap().retain(|footprint| {
                                !request
                                    .item_ids
                                    .iter()
                                    .any(|id| id.value == footprint_kiid(footprint))
                            });
                        }
                        let deleted_items =
                            if reports_each_deletion.load(std::sync::atomic::Ordering::SeqCst) {
                                request
                                    .item_ids
                                    .iter()
                                    .map(|id| ItemDeletionResult {
                                        id: Some(id.clone()),
                                        status: if held.contains(&id.value) {
                                            ItemDeletionStatus::IdsOk
                                        } else {
                                            ItemDeletionStatus::IdsNonexistent
                                        } as i32,
                                    })
                                    .collect()
                            } else {
                                Vec::new()
                            };
                        return Some(copperpour_ipc::builders::pack_any(
                            &kiapi::common::commands::DeleteItemsResponse {
                                header: None,
                                status: kiapi::common::types::ItemRequestStatus::IrsOk as i32,
                                deleted_items,
                            },
                            "kiapi.common.commands.DeleteItemsResponse",
                        ));
                    }
                    if command.type_url.ends_with("EndCommit") {
                        if matches!(readback, Readback::RefuseCreation) {
                            let request = kiapi::common::commands::EndCommit::decode(
                                command.value.as_slice(),
                            )
                            .unwrap();
                            assert_eq!(
                                request.action(),
                                kiapi::common::commands::CommitAction::CmaDrop
                            );
                        }
                        committed.store(true, std::sync::atomic::Ordering::SeqCst);
                        return Some(copperpour_ipc::builders::pack_any(
                            &kiapi::common::commands::EndCommitResponse {},
                            "kiapi.common.commands.EndCommitResponse",
                        ));
                    }
                    None
                },
            );
            let handler = crate::mcp::handler::McpHandler::new(crate::tools::ServerConfig {
                kicad_cli: cli.to_string_lossy().to_string(),
                kicad_binary: String::new(),
                ipc_address: kicad.address().to_string(),
                project_dir: None,
                jlcpcb_db_path: None,
                auto_load_toolsets: false,
                eager_toolsets: true,
            })
            .await
            .expect("handler builds");
            Self {
                _temp: temp,
                _kicad: kicad,
                handler,
                schematic,
                board,
                exported,
                drc_report,
                zones_after_commit,
                created,
                held: held_footprints,
                board_text,
                commands,
                kicad_version,
                keeps_deleted,
                reports_deleted,
                fails_readback,
                vias: vias_held,
                zones: zones_held,
                connected,
                keeps_copper,
                fails_copper_readback,
            }
        }

        /// Have `GetVersion` answer `major.minor.patch` from now on.
        fn report_kicad_version(&self, major: u32, minor: u32, patch: u32) {
            *self.kicad_version.lock().unwrap() = (major, minor, patch);
        }

        /// Have `SaveDocumentToString` answer with `text` from now on.
        fn hold_board_text(&self, text: &str) {
            *self.board_text.lock().unwrap() = text.to_string();
        }

        /// Have the stand-in `kicad-cli pcb drc` write `report` from now on.
        fn hold_drc_report(&self, report: &str) {
            std::fs::write(&self.drc_report, report).unwrap();
        }

        /// Have the mock list `zones` once a commit has ended.
        fn list_zones_after_commit(&self, zones: Vec<prost_types::Any>) {
            *self.zones_after_commit.lock().unwrap() = Some(zones);
        }

        /// How many requests carried `command`.
        fn sent(&self, command: &str) -> usize {
            let commands = self.commands.lock().unwrap();
            commands.iter().filter(|name| *name == command).count()
        }

        /// One `tools/call`, with the result's `isError` beside the body.
        async fn call(&self, arguments: serde_json::Value) -> (bool, serde_json::Value) {
            let (is_error, text) = self.call_text(arguments).await;
            (is_error, serde_json::from_str(&text).unwrap())
        }

        /// As [`Self::call`], with the body as the text the client receives.
        async fn call_text(&self, arguments: serde_json::Value) -> (bool, String) {
            let response = self
                .handler
                .handle_message(serde_json::json!({
                    "jsonrpc": "2.0",
                    "id": 789,
                    "method": "tools/call",
                    "params": { "name": "update_pcb_from_schematic", "arguments": arguments }
                }))
                .await
                .expect("tools/call receives a response");
            let result = response.result.expect("successful JSON-RPC response");
            (
                result["isError"] == serde_json::json!(true),
                result["content"][0]["text"].as_str().unwrap().to_string(),
            )
        }

        /// A sync of `netlist` through `tools/call` with `extra` beside the
        /// paths, as the text the client receives.
        async fn sync_text(&self, netlist: &str, extra: serde_json::Value) -> (bool, String) {
            std::fs::write(&self.exported, netlist).unwrap();
            let mut arguments = serde_json::json!({
                "schematic": self.schematic.to_string_lossy(),
                "board": self.board.to_string_lossy(),
            });
            let serde_json::Value::Object(extra) = extra else {
                panic!("extra arguments are an object")
            };
            arguments.as_object_mut().unwrap().extend(extra);
            self.call_text(arguments).await
        }

        /// As [`Self::sync_text`], parsed.
        async fn sync(&self, netlist: &str, extra: serde_json::Value) -> (bool, serde_json::Value) {
            let (is_error, text) = self.sync_text(netlist, extra).await;
            (is_error, serde_json::from_str(&text).unwrap())
        }

        /// A dry run and then the apply of its plan, both through
        /// `tools/call`. Returns the apply's body.
        async fn apply(&self, netlist: &str) -> serde_json::Value {
            let plan = self.dry_run(netlist).await;
            assert_eq!(plan["status"], "ready", "{plan:#}");
            let (is_error, applied) = self
                .call(serde_json::json!({
                    "schematic": self.schematic.to_string_lossy(),
                    "board": self.board.to_string_lossy(),
                    "dry_run": false,
                    "expected_plan_revision": plan["plan_revision"],
                }))
                .await;
            assert!(!is_error, "{applied:#}");
            applied
        }

        /// The footprints `CreateItems` received.
        fn sent_footprints(
            &self,
        ) -> Vec<copperpour_ipc::gen::kiapi::board::types::FootprintInstance> {
            self.created
                .lock()
                .unwrap()
                .iter()
                .map(|item| {
                    copperpour_ipc::gen::kiapi::board::types::FootprintInstance::decode(
                        item.value.as_slice(),
                    )
                    .expect("a footprint")
                })
                .collect()
        }

        /// A dry run through `tools/call`, for a schematic exporting `netlist`.
        async fn dry_run(&self, netlist: &str) -> serde_json::Value {
            self.dry_run_result(netlist).await.1
        }

        /// As [`Self::dry_run`], with the result's `isError` beside the body.
        async fn dry_run_result(&self, netlist: &str) -> (bool, serde_json::Value) {
            std::fs::write(&self.exported, netlist).unwrap();
            let response = self
                .handler
                .handle_message(serde_json::json!({
                    "jsonrpc": "2.0",
                    "id": 657,
                    "method": "tools/call",
                    "params": {
                        "name": "update_pcb_from_schematic",
                        "arguments": {
                            "schematic": self.schematic.to_string_lossy(),
                            "board": self.board.to_string_lossy()
                        }
                    }
                }))
                .await
                .expect("tools/call receives a response");
            let result = response.result.expect("successful JSON-RPC response");
            (
                result["isError"] == serde_json::json!(true),
                serde_json::from_str(result["content"][0]["text"].as_str().unwrap()).unwrap(),
            )
        }
    }

    /// A created item as the mock's board lists it back.
    fn read_back(item: &prost_types::Any, readback: Readback) -> prost_types::Any {
        match readback {
            Readback::AsSent | Readback::RefuseCreation => item.clone(),
            Readback::WithoutLibraryData => {
                let mut footprint =
                    copperpour_ipc::gen::kiapi::board::types::FootprintInstance::decode(
                        item.value.as_slice(),
                    )
                    .expect("a footprint");
                footprint.attributes = None;
                if let Some(definition) = footprint.definition.as_mut() {
                    definition.items.retain(|child| {
                        !copperpour_ipc::builders::any_is(
                            child,
                            "kiapi.board.types.Footprint3DModel",
                        )
                    });
                }
                copperpour_ipc::builders::pack_any(
                    &footprint,
                    "kiapi.board.types.FootprintInstance",
                )
            }
            Readback::WithoutPadProperties | Readback::UnknownPadPropertiesAsNone => {
                use copperpour_ipc::gen::kiapi::board::types::{
                    FootprintInstance, Pad, PadFabricationProperty,
                };
                let mut footprint =
                    FootprintInstance::decode(item.value.as_slice()).expect("a footprint");
                for child in &mut footprint.definition.as_mut().unwrap().items {
                    if copperpour_ipc::builders::any_is(child, "kiapi.board.types.Pad") {
                        let mut pad = Pad::decode(child.value.as_slice()).unwrap();
                        pad.fab_property = match readback {
                            Readback::WithoutPadProperties => 0,
                            _ if pad.fab_property == 0 => PadFabricationProperty::PfpNone as i32,
                            _ => pad.fab_property,
                        };
                        *child = copperpour_ipc::builders::pack_any(&pad, "kiapi.board.types.Pad");
                    }
                }
                copperpour_ipc::builders::pack_any(
                    &footprint,
                    "kiapi.board.types.FootprintInstance",
                )
            }
            Readback::WithoutDnp => {
                let mut footprint =
                    copperpour_ipc::gen::kiapi::board::types::FootprintInstance::decode(
                        item.value.as_slice(),
                    )
                    .expect("a footprint");
                if let Some(attributes) = footprint.attributes.as_mut() {
                    attributes.do_not_populate = false;
                }
                copperpour_ipc::builders::pack_any(
                    &footprint,
                    "kiapi.board.types.FootprintInstance",
                )
            }
            Readback::WithoutSheet => {
                let mut footprint =
                    copperpour_ipc::gen::kiapi::board::types::FootprintInstance::decode(
                        item.value.as_slice(),
                    )
                    .expect("a footprint");
                footprint.symbol_sheet_name.clear();
                footprint.symbol_sheet_filename.clear();
                footprint.symbol_footprint_filters.clear();
                copperpour_ipc::builders::pack_any(
                    &footprint,
                    "kiapi.board.types.FootprintInstance",
                )
            }
            Readback::WithoutLibraryFields => {
                use copperpour_ipc::gen::kiapi::board::types::BoardLayer;
                let mut footprint =
                    copperpour_ipc::gen::kiapi::board::types::FootprintInstance::decode(
                        item.value.as_slice(),
                    )
                    .expect("a footprint");
                if let Some(value) = footprint.value_field.as_mut() {
                    value.visible = false;
                    if let Some(text) = value.text.as_mut() {
                        text.layer = BoardLayer::BlFSilkS as i32;
                    }
                }
                if let Some(attributes) = footprint
                    .reference_field
                    .as_mut()
                    .and_then(|field| field.text.as_mut())
                    .and_then(|text| text.text.as_mut())
                    .and_then(|text| text.attributes.as_mut())
                {
                    attributes.keep_upright = false;
                }
                if let Some(definition) = footprint.definition.as_mut() {
                    definition.items.retain(|child| {
                        !copperpour_ipc::builders::any_is(child, "kiapi.board.types.Field")
                    });
                }
                copperpour_ipc::builders::pack_any(
                    &footprint,
                    "kiapi.board.types.FootprintInstance",
                )
            }
        }
    }

    /// `netlist` with `reference` marked DNP the way `kicad-cli` exports it.
    fn with_dnp(netlist: &str, reference: &str) -> String {
        // Anchored on the component entry: the net nodes name the reference too.
        let anchor = format!("(comp\n      (ref \"{reference}\")");
        assert_eq!(
            netlist.matches(&anchor).count(),
            1,
            "{reference} is exported once"
        );
        netlist.replace(
            &anchor,
            &format!("{anchor}\n      (property (name \"dnp\"))"),
        )
    }

    /// #688 through the served boundary: a part the board lacks is staged
    /// 5 mm to the right of what KiCad holds, measured item by item.
    ///
    /// KiCad answers `GetBoundingBox` only for the KIIDs a request names. The
    /// snapshot used to send an empty request, read the empty answer as an
    /// empty board, and stage every addition beside the page origin instead.
    #[tokio::test]
    async fn additions_are_staged_beside_the_board_kicad_holds() {
        // An outline away from the origin, as a real board is.
        let served = ServedSync::holding_outline(Some((100.0, 80.0, 50.0, 40.0))).await;

        let plan = served
            .dry_run(&exported_netlist(&[("C1", STOCK_0603, &["1", "2"])]))
            .await;

        assert_eq!(plan["status"], "ready", "{plan:#}");
        let added = &plan["changes"][0];
        assert_eq!(added["kind"], "add", "{plan:#}");
        let x = added["position"]["x"].as_f64().unwrap();
        let y = added["position"]["y"].as_f64().unwrap();
        // The staging column starts 5 mm past the board's right edge (150 mm)
        // and the first part is centred half its width further on.
        assert!(x > 155.0 && x < 165.0, "staged at x = {x}: {plan:#}");
        // Stacked down from the board's top edge (80 mm), not from y = 0.
        assert!(y > 80.0 && y < 90.0, "staged at y = {y}: {plan:#}");
    }

    /// Served dry run of `R1` ([`schematic_backed_resistor`]) on a board
    /// that also holds `zones`, after the schematic moves its pad on `net`
    /// to the copper-less `NEW_NET`.
    async fn plan_moving_off(net: &str, zones: Vec<prost_types::Any>) -> serde_json::Value {
        let served = ServedSync::holding(HeldBoard {
            footprints: vec![schematic_backed_resistor()],
            zones,
            nets: vec!["VCC", "GND"],
        })
        .await;
        served.dry_run(&netlist_moving_off(net)).await
    }

    /// [`ONE_RESISTOR`] for the captured `R1`, with its pad on `net` moved to
    /// the copper-less `NEW_NET`.
    fn netlist_moving_off(net: &str) -> String {
        ONE_RESISTOR
            .replace("Resistor_SMD:R_0603_1608Metric", "Resistor_SMD:R_0402")
            .replace("/Power/VCC", "VCC")
            .replace(&format!("(name \"{net}\")"), "(name \"NEW_NET\")")
    }

    /// The messages of a served dry run's `routed_pad_net_change` conflicts.
    fn routed_pad_conflicts(plan: &serde_json::Value) -> Vec<String> {
        plan["diagnostics"]
            .as_array()
            .into_iter()
            .flatten()
            .filter(|diagnostic| diagnostic["code"] == "routed_pad_net_change")
            .map(|diagnostic| diagnostic["message"].as_str().unwrap().to_string())
            .collect()
    }

    /// #779: a copper zone counts as copper on its own net only. KiCad's
    /// captured messages carry the pour's net and the rule area's lack of one.
    #[test]
    fn a_zone_pours_only_its_own_net() {
        let ZoneNet::Copper(net) = zone_net(GND_POUR) else {
            panic!("the captured pour is a copper zone");
        };
        assert_eq!(net.name, "GND");
        assert_eq!(zone_net(RULE_AREA), ZoneNet::RuleArea);
        assert_eq!(zone_net(&pour_without_a_net().value), ZoneNet::Unreadable);
        assert_eq!(zone_net(&[0xff, 0xff]), ZoneNet::Unreadable);
    }

    /// #779 through `tools/call`: a `GND` pour and a rule area on the board
    /// leave a pad on `VCC`, which has no copper, free to change net.
    #[tokio::test]
    async fn a_pour_does_not_block_a_net_it_does_not_carry() {
        let plan = plan_moving_off("VCC", vec![zone_item(GND_POUR), zone_item(RULE_AREA)]).await;

        assert_eq!(plan["status"], "ready", "{plan:#}");
        assert_eq!(routed_pad_conflicts(&plan), Vec::<String>::new());
        let update = &plan["changes"][0];
        assert_eq!(update["kind"], "update", "{plan:#}");
        assert_eq!(update["reference"], "R1", "{plan:#}");
    }

    /// The pour's own net still conflicts: moving `R1`'s `GND` pad would
    /// leave it under copper KiCad pours as `GND`.
    #[tokio::test]
    async fn a_pour_still_blocks_its_own_net() {
        let plan = plan_moving_off("GND", vec![zone_item(GND_POUR)]).await;

        assert_eq!(plan["status"], "conflict", "{plan:#}");
        assert_eq!(
            routed_pad_conflicts(&plan),
            vec!["R1 pad 2 would change from 'GND' to 'NEW_NET' while routed copper uses that net"],
        );
    }

    /// Moving a pad onto the pour's net conflicts too: the check covers the
    /// new net as well as the old one.
    #[tokio::test]
    async fn a_pour_blocks_a_pad_moving_onto_its_net() {
        let served = ServedSync::holding(HeldBoard {
            footprints: vec![schematic_backed_resistor()],
            zones: vec![zone_item(GND_POUR)],
            nets: vec!["VCC", "GND"],
        })
        .await;
        let netlist = ONE_RESISTOR
            .replace("Resistor_SMD:R_0603_1608Metric", "Resistor_SMD:R_0402")
            .replace("(name \"/Power/VCC\")", "(name \"GND\")");

        let plan = served.dry_run(&netlist).await;

        assert_eq!(plan["status"], "conflict", "{plan:#}");
        assert_eq!(
            routed_pad_conflicts(&plan),
            vec!["R1 pad 1 would change from 'VCC' to 'GND' while routed copper uses that net"],
        );
    }

    /// The plan #779 unblocks applies: KiCad's board ends with `R1` pad 1 on
    /// `NEW_NET` under the `GND` pour it was refused for before.
    #[tokio::test]
    async fn a_pad_under_a_pour_on_another_net_is_reassigned() {
        let served = ServedSync::holding(HeldBoard {
            footprints: vec![schematic_backed_resistor()],
            zones: vec![zone_item(GND_POUR), zone_item(RULE_AREA)],
            nets: vec!["VCC", "GND"],
        })
        .await;

        let applied = served.apply(&netlist_moving_off("VCC")).await;

        assert_eq!(applied["status"], "applied", "{applied:#}");
        let held = served.held.lock().unwrap();
        let footprint = copperpour_ipc::gen::kiapi::board::types::FootprintInstance::decode(
            held[0].value.as_slice(),
        )
        .expect("the board's R1");
        let r1 = board_footprint_from_instance(&footprint).expect("R1 identity");
        assert_eq!(r1.pad_nets.get("1").map(String::as_str), Some("NEW_NET"));
        assert_eq!(r1.pad_nets.get("2").map(String::as_str), Some("GND"));
    }

    /// A copper zone whose net cannot be read could pour any net, so the
    /// board still fails closed for every one of them.
    #[tokio::test]
    async fn a_pour_without_a_net_blocks_every_net() {
        let plan = plan_moving_off("VCC", vec![zone_item(GND_POUR), pour_without_a_net()]).await;

        assert_eq!(plan["status"], "conflict", "{plan:#}");
        assert_eq!(
            routed_pad_conflicts(&plan),
            vec!["R1 pad 1 would change from 'VCC' to 'NEW_NET' while routed copper uses that net"],
            "{plan:#}"
        );
    }

    /// KiCad's captured `R1` under its `GND` pour, with the stand-in DRC
    /// reporting one short.
    async fn served_r1_under_the_gnd_pour() -> ServedSync {
        let served = ServedSync::holding(HeldBoard {
            footprints: vec![schematic_backed_resistor()],
            zones: vec![zone_item(GND_POUR)],
            nets: vec!["VCC", "GND"],
        })
        .await;
        served.hold_drc_report(
            r#"{"violations":[{"type":"shorting_items","severity":"error","description":"Items shorting two nets"}],"unconnected_items":[],"schematic_parity":[]}"#,
        );
        served
    }

    /// Each `routed_pad_net_change` of a served dry run, as an entry of
    /// `acknowledged_net_changes`.
    fn acknowledging_each(plan: &serde_json::Value) -> serde_json::Value {
        plan["diagnostics"]
            .as_array()
            .into_iter()
            .flatten()
            .filter(|diagnostic| diagnostic["code"] == "routed_pad_net_change")
            .map(|diagnostic| {
                let change = &diagnostic["net_change"];
                serde_json::json!({
                    "reference": diagnostic["reference"],
                    "pad": change["pad"],
                    "from": change["from"],
                    "to": change["to"],
                })
            })
            .collect()
    }

    /// R1's pad 2 net on the mock's board.
    fn held_r1_pad_2(served: &ServedSync) -> Option<String> {
        let held = served.held.lock().unwrap();
        let footprint = copperpour_ipc::gen::kiapi::board::types::FootprintInstance::decode(
            held[0].value.as_slice(),
        )
        .expect("the board's R1");
        board_footprint_from_instance(&footprint)
            .expect("R1 identity")
            .pad_nets
            .get("2")
            .cloned()
    }

    /// #38 through `tools/call`: the conflict names the change; a dry run
    /// that acknowledges it plans the update; the apply of that plan moves
    /// R1's pad 2 off the pour's net and reports the copper on both nets and
    /// DRC of the board KiCad then holds. The mock lists the pour on the new
    /// net once the commit ends, so only a readback after it reports that.
    #[tokio::test]
    async fn an_acknowledged_routed_pad_net_change_is_applied_and_checked() {
        let served = served_r1_under_the_gnd_pour().await;
        served.list_zones_after_commit(vec![pour_on("NEW_NET")]);
        let netlist = netlist_moving_off("GND");

        let refused = served.dry_run(&netlist).await;
        assert_eq!(refused["status"], "conflict", "{refused:#}");
        let diagnostic = &refused["diagnostics"][0];
        assert_eq!(diagnostic["code"], "routed_pad_net_change", "{refused:#}");
        assert_eq!(diagnostic["reference"], "R1", "{refused:#}");
        assert_eq!(
            diagnostic["net_change"],
            serde_json::json!({ "pad": "2", "from": "GND", "to": "NEW_NET" }),
            "{refused:#}"
        );
        let acknowledged = acknowledging_each(&refused);

        let (_, plan) = served
            .sync(
                &netlist,
                serde_json::json!({ "acknowledged_net_changes": acknowledged }),
            )
            .await;
        assert_eq!(plan["status"], "ready", "{plan:#}");
        assert_eq!(plan["changes"][0]["kind"], "update", "{plan:#}");
        assert_eq!(plan["changes"][0]["pad_nets"]["2"], "NEW_NET", "{plan:#}");
        let noted = &plan["diagnostics"][0];
        assert_eq!(noted["code"], "routed_pad_net_change_acknowledged");
        assert_eq!(noted["net_change"], diagnostic["net_change"]);
        assert!(plan.get("routed_copper").is_none(), "{plan:#}");
        assert!(plan.get("drc").is_none(), "{plan:#}");

        let (is_error, applied) = served
            .sync(
                &netlist,
                serde_json::json!({
                    "dry_run": false,
                    "expected_plan_revision": plan["plan_revision"],
                    "acknowledged_net_changes": acknowledged,
                }),
            )
            .await;

        assert!(!is_error, "{applied:#}");
        assert_eq!(applied["status"], "applied", "{applied:#}");
        assert_eq!(held_r1_pad_2(&served).as_deref(), Some("NEW_NET"));
        assert_eq!(
            applied["routed_copper"],
            serde_json::json!([
                { "net": "GND", "before_count": 1, "after_count": 0 },
                { "net": "NEW_NET", "before_count": 0, "after_count": 1 },
            ]),
            "{applied:#}"
        );
        assert_eq!(
            applied["drc"],
            serde_json::json!({
                "clean": false,
                "violations": 1,
                "unconnected_items": 0,
                "schematic_parity_violations": 0,
                "project_rules_applied": cfg!(unix),
                "creepage_skipped": false,
            }),
            "{applied:#}"
        );
    }

    /// #38: the acknowledged plan applies only with its own revision and its
    /// acknowledgement. The conflict's revision is stale for it, and its
    /// revision without the acknowledgement is the conflict again; neither
    /// opens a commit.
    #[tokio::test]
    async fn an_acknowledged_plan_applies_only_with_its_revision_and_acknowledgement() {
        let served = served_r1_under_the_gnd_pour().await;
        let netlist = netlist_moving_off("GND");
        let refused = served.dry_run(&netlist).await;
        let acknowledged = acknowledging_each(&refused);
        let (_, plan) = served
            .sync(
                &netlist,
                serde_json::json!({ "acknowledged_net_changes": acknowledged }),
            )
            .await;
        assert_ne!(plan["plan_revision"], refused["plan_revision"]);

        for (arguments, code) in [
            (
                serde_json::json!({
                    "dry_run": false,
                    "expected_plan_revision": refused["plan_revision"],
                    "acknowledged_net_changes": acknowledged,
                }),
                "stale_plan_revision",
            ),
            (
                serde_json::json!({
                    "dry_run": false,
                    "expected_plan_revision": plan["plan_revision"],
                }),
                "routed_pad_net_change",
            ),
        ] {
            let (_, body) = served.sync(&netlist, arguments).await;
            assert_eq!(body["status"], "conflict", "{body:#}");
            assert!(
                body["diagnostics"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .any(|diagnostic| diagnostic["code"] == code),
                "{body:#}"
            );
        }
        assert_eq!(served.sent("BeginCommit"), 0);
        assert_eq!(held_r1_pad_2(&served).as_deref(), Some("GND"));
    }

    /// #38: the commit stands when DRC cannot run after it. The response is
    /// `applied`, flagged as an error, and says not to apply the plan again.
    #[tokio::test]
    async fn a_check_that_fails_after_the_commit_is_reported_on_the_applied_response() {
        let served = served_r1_under_the_gnd_pour().await;
        std::fs::remove_file(&served.drc_report).unwrap();
        let netlist = netlist_moving_off("GND");
        let acknowledged = acknowledging_each(&served.dry_run(&netlist).await);
        let (_, plan) = served
            .sync(
                &netlist,
                serde_json::json!({ "acknowledged_net_changes": acknowledged }),
            )
            .await;

        let (is_error, applied) = served
            .sync(
                &netlist,
                serde_json::json!({
                    "dry_run": false,
                    "expected_plan_revision": plan["plan_revision"],
                    "acknowledged_net_changes": acknowledged,
                }),
            )
            .await;

        assert!(is_error, "{applied:#}");
        assert_eq!(applied["status"], "applied", "{applied:#}");
        assert_eq!(served.sent("EndCommit"), 1);
        assert_eq!(held_r1_pad_2(&served).as_deref(), Some("NEW_NET"));
        let last = applied["diagnostics"].as_array().unwrap().last().unwrap();
        assert_eq!(last["code"], "acknowledged_apply_unchecked", "{applied:#}");
        assert!(
            last["message"]
                .as_str()
                .unwrap()
                .contains("Do not apply this plan again"),
            "{applied:#}"
        );
        assert!(applied.get("drc").is_none(), "{applied:#}");
    }

    /// #38: a copper zone whose net KiCad cannot serve could carry either
    /// net, so the readback after the commit cannot count them. The apply
    /// stands and is reported unchecked, never with counts that leave it out.
    #[tokio::test]
    async fn a_zone_read_back_without_a_net_leaves_the_apply_unchecked() {
        let served = ServedSync::holding(HeldBoard {
            footprints: vec![schematic_backed_resistor()],
            zones: vec![pour_without_a_net()],
            nets: vec!["VCC", "GND"],
        })
        .await;
        served.hold_drc_report(r#"{"violations":[],"unconnected_items":[],"schematic_parity":[]}"#);
        let netlist = netlist_moving_off("VCC");
        let acknowledged = acknowledging_each(&served.dry_run(&netlist).await);
        assert_eq!(acknowledged.as_array().map(Vec::len), Some(1));
        let (_, plan) = served
            .sync(
                &netlist,
                serde_json::json!({ "acknowledged_net_changes": acknowledged }),
            )
            .await;

        let (is_error, applied) = served
            .sync(
                &netlist,
                serde_json::json!({
                    "dry_run": false,
                    "expected_plan_revision": plan["plan_revision"],
                    "acknowledged_net_changes": acknowledged,
                }),
            )
            .await;

        assert!(is_error, "{applied:#}");
        assert_eq!(applied["status"], "applied", "{applied:#}");
        let last = applied["diagnostics"].as_array().unwrap().last().unwrap();
        assert_eq!(last["code"], "acknowledged_apply_unchecked", "{applied:#}");
        assert!(applied.get("routed_copper").is_none(), "{applied:#}");
    }

    /// #38: an acknowledgement missing a field, or carrying one it does not
    /// have, is refused before anything is read.
    #[tokio::test]
    async fn a_malformed_acknowledgement_is_an_invalid_argument() {
        let served = served_r1_under_the_gnd_pour().await;
        for acknowledged in [
            serde_json::json!([{ "reference": "R1", "pad": "2", "from": "GND" }]),
            serde_json::json!([{
                "reference": "R1", "pad": "2", "from": "GND", "to": "NEW_NET", "code": "x"
            }]),
            serde_json::json!({ "reference": "R1" }),
        ] {
            let (is_error, refused) = served
                .sync(
                    &netlist_moving_off("GND"),
                    serde_json::json!({ "acknowledged_net_changes": acknowledged }),
                )
                .await;
            assert!(is_error, "{refused:#}");
            assert_eq!(refused["error"]["kind"], "invalid_argument", "{refused:#}");
            assert!(
                refused["error"]["field"]
                    .as_str()
                    .unwrap()
                    .starts_with("acknowledged_net_changes"),
                "{refused:#}"
            );
        }
        assert_eq!(served.sent("GetItems"), 0);
    }

    /// The handler refuses a malformed acknowledgement itself, for a caller
    /// that reaches it without the served schema.
    #[tokio::test]
    async fn the_handler_refuses_a_malformed_acknowledgement_itself() {
        let context = ToolContext::new(
            crate::tools::ServerConfig {
                kicad_cli: String::new(),
                kicad_binary: String::new(),
                ipc_address: String::new(),
                project_dir: None,
                jlcpcb_db_path: None,
                auto_load_toolsets: false,
                eager_toolsets: false,
            },
            std::sync::Arc::new(crate::tools::ToolRouter::new()),
        );
        for acknowledged in [
            serde_json::json!([{ "reference": "R1", "pad": "2" }]),
            serde_json::json!([{
                "reference": "R1", "pad": "2", "from": "GND", "to": "NEW_NET", "code": "x"
            }]),
        ] {
            let result = handle_update_pcb_from_schematic(
                &serde_json::json!({
                    "schematic": "/absent/carrier.kicad_sch",
                    "board": "/absent/carrier.kicad_pcb",
                    "acknowledged_net_changes": acknowledged,
                }),
                &context,
            )
            .await
            .unwrap();

            assert!(result.is_error);
            let ToolContent::Text { text } = &result.content[0] else {
                panic!("the refusal is JSON text");
            };
            let refused: serde_json::Value = serde_json::from_str(text).unwrap();
            assert_eq!(refused["error"]["kind"], "invalid_argument", "{refused:#}");
            assert_eq!(
                refused["error"]["field"], "acknowledged_net_changes",
                "{refused:#}"
            );
        }
    }

    /// The classes KiCad 10.0.5 refuses to list on every board.
    fn kicad_10_refusals() -> Vec<copperpour_ipc::gen::kiapi::common::types::KiCadObjectType> {
        use copperpour_ipc::gen::kiapi::common::types::KiCadObjectType as Kind;
        vec![Kind::KotPcbTable, Kind::KotPcbGenerator]
    }

    /// One refused class, as `staging.unavailable_item_classes` reports it.
    fn refused_class(class: &str) -> serde_json::Value {
        serde_json::json!({
            "class": class,
            "reason": "refused",
            "kiapi_status": "AS_BAD_REQUEST",
            "message": crate::tools::pcb_board::board_mock::KICAD_CLASS_REFUSAL,
        })
    }

    /// #688's disclosure in its sync consumer. On a board KiCad 10.0.5 holds,
    /// additions are staged beside what KiCad measured, and the dry run says
    /// that is not the whole board: tables and generators were not listed.
    #[tokio::test]
    async fn staging_names_the_classes_kicad_would_not_list() {
        let served =
            ServedSync::refusing(Some((100.0, 80.0, 50.0, 40.0)), kicad_10_refusals()).await;

        let plan = served
            .dry_run(&exported_netlist(&[("C1", STOCK_0603, &["1", "2"])]))
            .await;

        assert_eq!(plan["status"], "ready", "{plan:#}");
        assert_eq!(
            plan["staging"],
            serde_json::json!({
                "basis": "partial_geometry",
                "unavailable_item_classes": [refused_class("tables"), refused_class("generators")],
            }),
            "{plan:#}"
        );
        // Still staged beside the measured outline.
        let x = plan["changes"][0]["position"]["x"].as_f64().unwrap();
        assert!(x > 155.0 && x < 165.0, "staged at x = {x}: {plan:#}");
    }

    /// On KiCad 10.0.7+, tables are listed and measured, so additions are
    /// staged to the right of a table extending past the board outline and
    /// `staging.unavailable_item_classes` names only generators.
    #[tokio::test]
    async fn staging_measures_tables_when_kicad_lists_them() {
        use copperpour_ipc::gen::kiapi::common::types::KiCadObjectType as Kind;

        let served = ServedSync::build(
            Some((100.0, 80.0, 50.0, 40.0)),
            vec![("board-table", (200.0, 80.0, 30.0, 20.0))],
            vec![Kind::KotPcbGenerator],
            Readback::AsSent,
            HeldBoard::default(),
        )
        .await;

        let plan = served
            .dry_run(&exported_netlist(&[("C1", STOCK_0603, &["1", "2"])]))
            .await;

        assert_eq!(plan["status"], "ready", "{plan:#}");
        assert_eq!(
            plan["staging"],
            serde_json::json!({
                "basis": "partial_geometry",
                "unavailable_item_classes": [refused_class("generators")],
            }),
            "{plan:#}"
        );
        // Staged past the table's right edge (230 mm), not the outline's (150 mm).
        let x = plan["changes"][0]["position"]["x"].as_f64().unwrap();
        assert!(x > 235.0 && x < 245.0, "staged at x = {x}: {plan:#}");
    }

    /// A board where KiCad measured nothing but would not list some classes is
    /// not called empty: it may hold tables or generators. Staging still starts
    /// at the origin, and the response says why.
    #[tokio::test]
    async fn a_board_with_nothing_measured_is_not_reported_empty() {
        let served = ServedSync::refusing(None, kicad_10_refusals()).await;

        let plan = served
            .dry_run(&exported_netlist(&[("C1", STOCK_0603, &["1", "2"])]))
            .await;

        assert_eq!(plan["status"], "ready", "{plan:#}");
        assert_eq!(plan["staging"]["basis"], "no_measured_geometry", "{plan:#}");
        assert_eq!(
            plan["staging"]["unavailable_item_classes"],
            serde_json::json!([refused_class("tables"), refused_class("generators")])
        );
        let x = plan["changes"][0]["position"]["x"].as_f64().unwrap();
        assert!(
            x > 5.0 && x < 15.0,
            "staged from the origin, x = {x}: {plan:#}"
        );
    }

    /// The controls: a KiCad that lists every class reports a board it measured
    /// as complete, and one holding nothing as empty, with no classes named.
    #[tokio::test]
    async fn a_board_whose_every_class_was_listed_is_complete_or_empty() {
        let measured = ServedSync::holding_outline(Some((100.0, 80.0, 50.0, 40.0))).await;
        let plan = measured
            .dry_run(&exported_netlist(&[("C1", STOCK_0603, &["1", "2"])]))
            .await;
        assert_eq!(
            plan["staging"],
            serde_json::json!({ "basis": "complete_geometry", "unavailable_item_classes": [] }),
            "{plan:#}"
        );

        let empty = ServedSync::new().await;
        let plan = empty
            .dry_run(&exported_netlist(&[("C1", STOCK_0603, &["1", "2"])]))
            .await;
        assert_eq!(
            plan["staging"],
            serde_json::json!({ "basis": "empty_board", "unavailable_item_classes": [] }),
            "{plan:#}"
        );
    }

    /// A refusal before the board was read has no staging to report, and
    /// says so with `null` rather than a basis it never measured.
    #[test]
    fn a_plan_that_never_reached_the_board_reports_no_staging() {
        let plan = plan_adding(Vec::new());
        let body = match &sync_response(&plan, "conflict", 1, false, &ResponseOptions::default())
            .content[0]
        {
            ToolContent::Text { text } => serde_json::from_str::<serde_json::Value>(text).unwrap(),
            _ => panic!("sync response was not JSON text"),
        };
        assert_eq!(body["staging"], serde_json::Value::Null);
    }

    /// The two diagnostics for the unusable VQFN footprints, as #657 names
    /// them: every part and both footprints.
    fn unusable_vqfn_diagnostics() -> serde_json::Value {
        serde_json::json!([
            {
                "code": "unsupported_library_footprint",
                "message": format!(
                    "{TEXAS_VQFN} cannot be placed (needed by U1, U2): custom pad '1': its \
                     (clearance convexhull) has no field in KiCad 10's pad message, which \
                     gives the pad the outline clearance"
                ),
                "reference": null,
                "references": ["U1", "U2"],
                "footprint_id": TEXAS_VQFN
            },
            {
                "code": "unsupported_library_footprint",
                "message": format!(
                    "{GENERIC_VQFN} cannot be placed (needed by U3): custom pad '1': its \
                     (clearance convexhull) has no field in KiCad 10's pad message, which \
                     gives the pad the outline clearance"
                ),
                "reference": "U3",
                "references": ["U3"],
                "footprint_id": GENERIC_VQFN
            }
        ])
    }

    /// #657 through the served boundary. The run that found it needed two
    /// unusable footprints and was told of one, with `reference: null` and no
    /// footprint: every part and both footprints are named now. Those parts
    /// are skipped, as KiCad's Update PCB skips a part it cannot add, and the
    /// part that can be placed is still planned.
    #[tokio::test]
    async fn two_unusable_footprints_are_both_named_and_skipped_through_the_served_dispatch() {
        let served = ServedSync::new().await;

        let plan = served
            .dry_run(&exported_netlist(&[
                ("U1", TEXAS_VQFN, &["1", "2"]),
                ("C1", STOCK_0603, &["1", "2"]),
                ("U3", GENERIC_VQFN, &["1", "2"]),
                ("U2", TEXAS_VQFN, &["1", "2"]),
            ]))
            .await;

        assert_eq!(plan["status"], "ready", "{plan:#}");
        assert_eq!(plan["changes_count"], 1, "{plan:#}");
        assert_eq!(plan["changes"][0]["reference"], "C1", "{plan:#}");
        assert_eq!(plan["coverage"]["footprints_added"]["planned"], 1);
        assert_eq!(plan["coverage"]["skipped_footprint"]["planned"], 3);
        assert_eq!(plan["coverage"]["conflicts"]["planned"], 0);
        assert_eq!(plan["skipped_footprints"], unusable_vqfn_diagnostics());
        assert_eq!(plan["diagnostics"], serde_json::json!([]));
    }

    /// With nothing left that can be placed there is nothing to apply, and the
    /// plan is refused whole, as it was before parts were skipped: the same
    /// diagnostics, as conflicts.
    #[tokio::test]
    async fn a_plan_of_only_unusable_footprints_is_still_refused() {
        let served = ServedSync::new().await;

        let refused = served
            .dry_run(&exported_netlist(&[
                ("U1", TEXAS_VQFN, &["1", "2"]),
                ("U3", GENERIC_VQFN, &["1", "2"]),
                ("U2", TEXAS_VQFN, &["1", "2"]),
            ]))
            .await;

        assert_eq!(refused["status"], "conflict", "{refused:#}");
        assert_eq!(refused["changes"], serde_json::json!([]));
        assert_eq!(refused["coverage"]["footprints_added"]["planned"], 0);
        assert_eq!(refused["coverage"]["skipped_footprint"]["planned"], 0);
        assert_eq!(refused["coverage"]["conflicts"]["planned"], 2);
        assert_eq!(refused["skipped_footprints"], serde_json::json!([]));
        assert_eq!(refused["diagnostics"], unusable_vqfn_diagnostics());
    }

    /// A dry run said `ready` for a part whose schematic connects a pad its
    /// footprint does not have, and the apply then failed its preflight. The
    /// dry run finds it by name, and that part is skipped: the apply places
    /// the other part alone and counts the skipped one.
    #[tokio::test]
    async fn a_part_missing_a_pad_is_skipped_and_the_rest_applied_through_the_served_dispatch() {
        let served = ServedSync::new().await;
        let netlist = exported_netlist(&[
            ("C1", STOCK_0603, &["1", "2", "3"]),
            ("C2", STOCK_0603, &["1", "2"]),
        ]);

        let plan = served.dry_run(&netlist).await;

        assert_eq!(plan["status"], "ready", "{plan:#}");
        assert_eq!(plan["changes_count"], 1, "{plan:#}");
        assert_eq!(plan["changes"][0]["reference"], "C2", "{plan:#}");
        assert_eq!(plan["diagnostics"], serde_json::json!([]), "{plan:#}");
        let skipped = plan["skipped_footprints"].as_array().unwrap();
        assert_eq!(skipped.len(), 1, "{plan:#}");
        assert_eq!(skipped[0]["code"], "footprint_pad_missing");
        assert_eq!(skipped[0]["reference"], "C1");
        assert_eq!(skipped[0]["references"], serde_json::json!(["C1"]));
        assert_eq!(skipped[0]["footprint_id"], STOCK_0603);
        assert_eq!(
            skipped[0]["message"],
            format!("the schematic connects C1 pad 3, which footprint {STOCK_0603} does not have")
        );

        let applied = served.apply(&netlist).await;
        assert_eq!(applied["status"], "applied", "{applied:#}");
        assert_eq!(
            applied["coverage"]["skipped_footprint"],
            serde_json::json!({ "planned": 1, "applied": 1 })
        );
        // The apply answer lists the skipped part as the dry run did (#596).
        assert_eq!(
            applied["skipped_footprints"], plan["skipped_footprints"],
            "{applied:#}"
        );
        assert_eq!(applied["coverage"]["footprints_added"]["applied"], 1);
        let sent = served.sent_footprints();
        assert_eq!(sent.len(), 1);
        assert_eq!(field_text(&sent[0].reference_field), "C2");
    }

    /// The captured R1 (`schematic_backed_resistor`, pads 1 and 2) at `value`
    /// on `footprint_id`, its pins 1 and 2 on VCC and GND, with `components`
    /// and `nets` added to the export.
    fn r1_netlist_with(value: &str, footprint_id: &str, components: &str, nets: &str) -> String {
        format!(
            r#"(export
  (components
    (comp (ref "R1") (value "{value}") (footprint "{footprint_id}")
      (sheetpath (names "/Power/") (tstamps "/sheet-uuid/"))
      (tstamps "symbol-uuid"))
{components}  )
  (nets
    (net (code "1") (name "VCC") (class "Default")
      (node (ref "R1") (pin "1") (pintype "passive")))
    (net (code "2") (name "GND") (class "Default")
      (node (ref "R1") (pin "2") (pintype "passive")))
{nets}  ))
"#
        )
    }

    /// R1's pin 3 alone on its net, as KiCad exports a pin with a
    /// no-connect flag.
    const R1_PIN_3_NO_CONNECT: &str = r#"    (net (code "3") (name "unconnected-(R1-Pad3)") (class "Default")
      (node (ref "R1") (pin "3") (pintype "passive+no_connect")))
"#;

    fn holding_r1() -> HeldBoard {
        HeldBoard {
            footprints: vec![schematic_backed_resistor()],
            zones: Vec::new(),
            nets: vec!["VCC", "GND"],
        }
    }

    /// A footprint on the board without a pad the schematic connects used to
    /// refuse the whole plan, unlike an added part without one. It is
    /// skipped now, as that part is, and everything else is applied.
    #[tokio::test]
    async fn a_board_footprint_missing_a_connected_pad_is_skipped_and_the_rest_applied() {
        let served = ServedSync::holding(holding_r1()).await;
        let netlist = r1_netlist_with(
            "10k",
            "Resistor_SMD:R_0402",
            &format!(
                r#"    (comp (ref "C1") (value "100n") (footprint "{STOCK_0603}")
      (sheetpath (names "/") (tstamps "/"))
      (tstamps "c1-uuid"))
"#
            ),
            r#"    (net (code "3") (name "SENSE") (class "Default")
      (node (ref "R1") (pin "3") (pintype "passive"))
      (node (ref "C1") (pin "1") (pintype "passive")))
    (net (code "4") (name "C1-2") (class "Default")
      (node (ref "C1") (pin "2") (pintype "passive")))
"#,
        );

        let plan = served.dry_run(&netlist).await;

        assert_eq!(plan["status"], "ready", "{plan:#}");
        assert_eq!(plan["diagnostics"], serde_json::json!([]), "{plan:#}");
        assert_eq!(plan["changes_count"], 1, "{plan:#}");
        assert_eq!(plan["changes"][0]["kind"], "add", "{plan:#}");
        assert_eq!(plan["changes"][0]["reference"], "C1", "{plan:#}");
        assert_eq!(
            plan["skipped_footprints"],
            serde_json::json!([{
                "code": "footprint_pad_missing",
                "message": "the schematic connects R1 pad 3, which footprint Resistor_SMD:R_0402 does not have",
                "reference": "R1",
                "references": ["R1"],
                "footprint_id": "Resistor_SMD:R_0402",
            }]),
            "{plan:#}"
        );
        assert_eq!(plan["coverage"]["skipped_footprint"]["planned"], 1);

        let applied = served.apply(&netlist).await;
        assert_eq!(applied["status"], "applied", "{applied:#}");
        assert_eq!(applied["skipped_footprints"], plan["skipped_footprints"]);
        assert_eq!(served.sent("UpdateItems"), 0, "R1 is left as it is");
        let sent = served.sent_footprints();
        assert_eq!(sent.len(), 1);
        assert_eq!(field_text(&sent[0].reference_field), "C1");
    }

    /// The real-board case: a keyed header's footprint has no pad at the
    /// key, where the schematic's pin has a no-connect flag. The footprint is
    /// updated, and the apply sets the nets of the pads it has.
    #[tokio::test]
    async fn a_keyed_board_footprint_is_updated_without_a_pad_at_its_key() {
        use copperpour_ipc::gen::kiapi::board::types::FootprintInstance;
        let served = ServedSync::holding(holding_r1()).await;
        let netlist = r1_netlist_with("10k", "Resistor_SMD:R_0402", "", R1_PIN_3_NO_CONNECT);

        let plan = served.dry_run(&netlist).await;

        assert_eq!(plan["status"], "ready", "{plan:#}");
        assert_eq!(plan["diagnostics"], serde_json::json!([]), "{plan:#}");
        assert_eq!(
            plan["skipped_footprints"],
            serde_json::json!([]),
            "{plan:#}"
        );
        assert_eq!(plan["changes_count"], 1, "{plan:#}");
        assert_eq!(plan["changes"][0]["kind"], "update", "{plan:#}");
        assert_eq!(plan["changes"][0]["value"], "10k", "{plan:#}");
        assert_eq!(
            plan["changes"][0]["pad_nets"],
            serde_json::json!({ "1": "VCC", "2": "GND" }),
            "{plan:#}"
        );
        assert_eq!(
            plan["coverage"]["pads_reassigned"]["planned"], 0,
            "{plan:#}"
        );

        let applied = served.apply(&netlist).await;
        assert_eq!(applied["status"], "applied", "{applied:#}");
        let held =
            FootprintInstance::decode(served.held.lock().unwrap()[0].value.as_slice()).unwrap();
        assert_eq!(field_text(&held.value_field), "10k");
    }

    /// An added footprint without a pad for a pin wired to nothing, which
    /// KiCad puts alone on an `unconnected-…` net, is placed, not skipped.
    #[tokio::test]
    async fn an_added_footprint_needs_no_pad_for_a_pin_kicad_left_unconnected() {
        let served = ServedSync::new().await;
        let netlist = format!(
            r#"(export
  (components
    (comp (ref "C1") (value "100n") (footprint "{STOCK_0603}")
      (sheetpath (names "/") (tstamps "/"))
      (tstamps "c1-uuid")))
  (nets
    (net (code "1") (name "VCC") (class "Default")
      (node (ref "C1") (pin "1") (pintype "passive")))
    (net (code "2") (name "GND") (class "Default")
      (node (ref "C1") (pin "2") (pintype "passive")))
    (net (code "3") (name "unconnected-(C1-Pad3)") (class "Default")
      (node (ref "C1") (pin "3") (pintype "passive")))))
"#
        );

        let plan = served.dry_run(&netlist).await;

        assert_eq!(plan["status"], "ready", "{plan:#}");
        assert_eq!(
            plan["skipped_footprints"],
            serde_json::json!([]),
            "{plan:#}"
        );
        assert_eq!(plan["changes"][0]["kind"], "add", "{plan:#}");
        assert_eq!(
            plan["changes"][0]["pad_nets"],
            serde_json::json!({ "1": "VCC", "2": "GND" }),
            "{plan:#}"
        );
        let applied = served.apply(&netlist).await;
        assert_eq!(applied["status"], "applied", "{applied:#}");
        assert_eq!(served.sent_footprints().len(), 1);
    }

    /// The same for a replacement: its new footprint needs no pad for a pin
    /// KiCad left unconnected.
    #[tokio::test]
    async fn a_replacement_needs_no_pad_for_a_pin_kicad_left_unconnected() {
        let served = ServedSync::holding(holding_r1()).await;
        let netlist = r1_netlist_with("10k", STOCK_0603, "", R1_PIN_3_NO_CONNECT);

        let plan = served.dry_run(&netlist).await;

        assert_eq!(plan["status"], "ready", "{plan:#}");
        assert_eq!(
            plan["skipped_footprints"],
            serde_json::json!([]),
            "{plan:#}"
        );
        assert_eq!(
            plan["changes"][0]["replace_footprint"]["to"], STOCK_0603,
            "{plan:#}"
        );
        assert_eq!(
            plan["changes"][0]["pad_nets"],
            serde_json::json!({ "1": "VCC", "2": "GND" }),
            "{plan:#}"
        );
        // Pads 1 and 2 keep their nets, and the new footprint has no pad 3.
        assert_eq!(
            plan["coverage"]["pads_reassigned"]["planned"], 0,
            "{plan:#}"
        );
        let applied = served.apply(&netlist).await;
        assert_eq!(applied["status"], "applied", "{applied:#}");
    }

    /// A refusal before any plan exists (saved hierarchy, netlist export, IPC
    /// preflight) went out with a hand-built diagnostic of only `code` and
    /// `message`, so the responses that say least were also the ones missing
    /// the fields every other diagnostic carries. One constructor builds them
    /// all now. The whole object is compared: indexing a missing key yields
    /// `null` as well, so a per-field check could not tell absent from null.
    #[tokio::test]
    async fn a_preflight_refusal_has_the_same_diagnostic_fields_through_the_served_dispatch() {
        let served = ServedSync::new().await;

        // An export with no components section fails the netlist preflight.
        let (is_error, refused) = served.dry_run_result("(export (version \"E\"))").await;

        assert!(is_error, "{refused:#}");
        assert_eq!(refused["status"], "conflict");
        let message = refused["diagnostics"][0]["message"].as_str().unwrap();
        assert!(message.starts_with("netlist preflight failed"), "{message}");
        assert_eq!(
            refused["diagnostics"],
            serde_json::json!([{
                "code": "preflight_conflict",
                "message": message,
                "reference": null,
                "references": [],
                "footprint_id": null
            }])
        );

        // The same keys as a diagnostic from planning, so one reader serves both.
        let planned = served
            .dry_run(&exported_netlist(&[("U1", TEXAS_VQFN, &["1"])]))
            .await;
        let keys = |diagnostic: &serde_json::Value| {
            diagnostic
                .as_object()
                .unwrap()
                .keys()
                .cloned()
                .collect::<BTreeSet<_>>()
        };
        assert_eq!(
            keys(&refused["diagnostics"][0]),
            keys(&planned["diagnostics"][0])
        );
    }

    #[tokio::test]
    async fn sync_reports_create_rejection_after_acknowledged_drop() {
        let served = ServedSync::reading_back(Readback::RefuseCreation).await;
        let netlist = exported_netlist(&[("C1", STOCK_0603, &["1", "2"])]);
        let before = std::fs::read(&served.board).unwrap();
        let plan = served.dry_run(&netlist).await;
        assert_eq!(plan["status"], "ready", "{plan}");
        let (is_error, result) = served
            .sync(
                &netlist,
                serde_json::json!({
                    "dry_run": false, "expected_plan_revision": plan["plan_revision"],
                }),
            )
            .await;
        assert!(is_error);
        let recovery = &result["recovery"];
        assert_eq!(recovery["cause"], "kicad_api_rejection", "{result}");
        assert_eq!(recovery["mutation_outcome"], "unknown");
        assert_eq!(recovery["evidence"]["rollback"], "confirmed");
        assert_eq!(
            recovery["evidence"]["rejected_items"][0]["status"],
            "ISC_INVALID_DATA"
        );
        assert_eq!(served.sent("CreateItems"), 1);
        assert_eq!(served.sent("EndCommit"), 1);
        assert_eq!(served.sent("UpdateItems"), 0);
        assert!(served.held.lock().unwrap().is_empty());
        assert_eq!(std::fs::read(&served.board).unwrap(), before);
    }

    /// #789 through the served boundary. A stock 0603 capacitor the board
    /// lacks is added, and what `CreateItems` receives carries the library's
    /// `(attr smd)`, its description and tags, and its 3D model. Before, the
    /// footprint went out with none of them, so KiCad treated the part as
    /// unspecified and `export_3d`'s default left it out of the STEP.
    #[tokio::test]
    async fn an_added_footprint_carries_its_library_attributes_text_and_model() {
        use copperpour_ipc::gen::kiapi;

        let served = ServedSync::reading_back(Readback::AsSent).await;
        let applied = served
            .apply(&exported_netlist(&[("C1", STOCK_0603, &["1", "2"])]))
            .await;

        assert_eq!(applied["status"], "applied", "{applied:#}");
        assert_eq!(applied["diagnostics"], serde_json::json!([]), "{applied:#}");
        let sent = served.sent_footprints();
        assert_eq!(sent.len(), 1);
        let footprint = &sent[0];
        assert_eq!(
            footprint.attributes.as_ref().map(|a| a.mounting_style()),
            Some(kiapi::board::types::FootprintMountingStyle::FmsSmd)
        );
        let definition = footprint.definition.as_ref().unwrap();
        let text = definition.attributes.as_ref().unwrap();
        assert!(
            text.description
                .starts_with("Capacitor SMD 0603 (1608 Metric)"),
            "{}",
            text.description
        );
        assert_eq!(text.keywords, "capacitor");
        let models: Vec<_> = definition
            .items
            .iter()
            .filter(|child| {
                copperpour_ipc::builders::any_is(child, "kiapi.board.types.Footprint3DModel")
            })
            .map(|child| {
                kiapi::board::types::Footprint3DModel::decode(child.value.as_slice()).unwrap()
            })
            .collect();
        assert_eq!(models.len(), 1);
        assert_eq!(
            models[0].filename,
            "${KICAD10_3DMODEL_DIR}/Capacitor_SMD.3dshapes/C_0603_1608Metric.step"
        );
        assert!(models[0].visible);
    }

    /// The readback holds the board to the mounting style and model files
    /// that were sent. A board that kept neither, which is what the sync used
    /// to build, is reported. It is not refused: the commit has already been
    /// applied, and the footprint is otherwise the one that was planned.
    #[tokio::test]
    async fn the_readback_names_library_data_the_board_did_not_keep() {
        let served = ServedSync::reading_back(Readback::WithoutLibraryData).await;
        let applied = served
            .apply(&exported_netlist(&[("C1", STOCK_0603, &["1", "2"])]))
            .await;

        assert_eq!(applied["status"], "applied", "{applied:#}");
        let messages: Vec<&str> = applied["diagnostics"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|diagnostic| diagnostic["code"] == "board_readback_differs")
            .map(|diagnostic| diagnostic["message"].as_str().unwrap())
            .collect();
        assert_eq!(messages.len(), 2, "{applied:#}");
        assert!(
            messages[0].contains("C1: sent attributes [smd], board now has []"),
            "{}",
            messages[0]
        );
        assert!(
            messages[1].contains(
                "C1: sent 3D models [${KICAD10_3DMODEL_DIR}/Capacitor_SMD.3dshapes/C_0603_1608Metric.step], board now has []"
            ),
            "{}",
            messages[1]
        );
    }

    /// One attribute lost while the mounting style and the models survive,
    /// which a check on those two alone passes. The flag is DNP as the
    /// schematic set it, the final value sent, not the library's: the 0603's
    /// library footprint is not DNP.
    #[tokio::test]
    async fn the_readback_names_a_single_attribute_the_board_did_not_keep() {
        let served = ServedSync::reading_back(Readback::WithoutDnp).await;
        let applied = served
            .apply(&with_dnp(
                &exported_netlist(&[("C1", STOCK_0603, &["1", "2"])]),
                "C1",
            ))
            .await;

        assert_eq!(applied["status"], "applied", "{applied:#}");
        assert_eq!(
            applied["diagnostics"].as_array().unwrap().len(),
            1,
            "{applied:#}"
        );
        assert_eq!(applied["diagnostics"][0]["code"], "board_readback_differs");
        let message = applied["diagnostics"][0]["message"].as_str().unwrap();
        assert!(
            message.contains("C1: sent attributes [dnp, smd], board now has [smd]"),
            "{message}"
        );
        assert!(
            served.sent_footprints()[0]
                .attributes
                .as_ref()
                .unwrap()
                .do_not_populate
        );
    }

    /// The control for the test above: the same DNP part on a board that kept
    /// everything is reported as nothing at all.
    #[tokio::test]
    async fn a_board_that_kept_every_attribute_reports_nothing() {
        let served = ServedSync::reading_back(Readback::AsSent).await;
        let applied = served
            .apply(&with_dnp(
                &exported_netlist(&[("C1", STOCK_0603, &["1", "2"])]),
                "C1",
            ))
            .await;

        assert_eq!(applied["status"], "applied", "{applied:#}");
        assert_eq!(applied["diagnostics"], serde_json::json!([]), "{applied:#}");
    }

    /// The sheet name, sheet file and footprint filters of each added
    /// footprint, by reference, as `CreateItems` received them.
    fn sent_sheets(served: &ServedSync) -> Vec<(String, String, String, String)> {
        served
            .sent_footprints()
            .iter()
            .map(|footprint| {
                (
                    field_text(&footprint.reference_field),
                    footprint.symbol_sheet_name.clone(),
                    footprint.symbol_sheet_filename.clone(),
                    footprint.symbol_footprint_filters.clone(),
                )
            })
            .collect()
    }

    /// #237 through the served boundary: each added footprint carries its
    /// sheet name, sheet file and footprint filters, the two channels one file
    /// under two names, and the plan lists them with the addition.
    #[tokio::test]
    async fn an_added_footprint_carries_its_sheet() {
        let served = ServedSync::reading_back(Readback::AsSent).await;
        let plan = served.dry_run(SHEET_FIELDS).await;
        let addition = &plan["changes"][1];
        assert_eq!(addition["reference"], "C2", "{plan:#}");
        assert_eq!(addition["sheet_name"], "/CH1/", "{plan:#}");
        assert_eq!(addition["sheet_file"], "channel.kicad_sch", "{plan:#}");
        assert_eq!(addition["footprint_filters"], "C_*", "{plan:#}");

        let applied = served.apply(SHEET_FIELDS).await;

        assert_eq!(applied["status"], "applied", "{applied:#}");
        assert_eq!(applied["diagnostics"], serde_json::json!([]), "{applied:#}");
        let sheet = |reference: &str, name: &str, file: &str| {
            (
                reference.to_string(),
                name.to_string(),
                file.to_string(),
                "C_*".to_string(),
            )
        };
        assert_eq!(
            sent_sheets(&served),
            vec![
                sheet("C1", "/", "sheet_fields.kicad_sch"),
                sheet("C2", "/CH1/", "channel.kicad_sch"),
                sheet("C3", "/CH2/", "channel.kicad_sch"),
            ]
        );
    }

    /// A board that did not keep a footprint's sheet is reported for that
    /// footprint, as any other part of it the board did not keep.
    #[tokio::test]
    async fn the_readback_names_a_sheet_the_board_did_not_keep() {
        let served = ServedSync::reading_back(Readback::WithoutSheet).await;
        let applied = served.apply(SHEET_FIELDS).await;

        assert_eq!(applied["status"], "applied", "{applied:#}");
        let messages = applied["diagnostics"]
            .as_array()
            .unwrap()
            .iter()
            .map(|diagnostic| {
                assert_eq!(diagnostic["code"], "board_readback_differs", "{applied:#}");
                diagnostic["message"].as_str().unwrap().to_string()
            })
            .collect::<Vec<_>>();
        assert_eq!(messages.len(), 3, "{applied:#}");
        assert!(
            messages[1].contains(
                "C2: sent sheet name \"/CH1/\", sheet file \"channel.kicad_sch\", \
                 footprint filters \"C_*\", board now has sheet name \"\", sheet file \"\", \
                 footprint filters \"\""
            ),
            "{}",
            messages[1]
        );
    }

    /// The schematic's R1 on `footprint_id`, on the captured R1's nets.
    fn r1_on(footprint_id: &str) -> String {
        ONE_RESISTOR
            .replace("Resistor_SMD:R_0603_1608Metric", footprint_id)
            .replace("/Power/VCC", "VCC")
    }

    /// KiCad's own R1 (`Resistor_SMD:R_0402`, linked to the `/Power/`
    /// symbol), its pads given the schematic pins Update PCB writes, a custom
    /// property its old library gave it, and the symbol's `MPN` field.
    fn r1_with_pins_and_an_old_property(
    ) -> copperpour_ipc::gen::kiapi::board::types::FootprintInstance {
        use copperpour_ipc::gen::kiapi;
        let mut footprint = kiapi::board::types::FootprintInstance::decode(
            schematic_backed_resistor().value.as_slice(),
        )
        .unwrap();
        let definition = footprint.definition.as_mut().unwrap();
        for item in &mut definition.items {
            if item.type_url.ends_with("kiapi.board.types.Pad") {
                let mut pad = kiapi::board::types::Pad::decode(item.value.as_slice()).unwrap();
                pad.symbol_pin = Some(kiapi::board::types::SymbolPinInfo {
                    name: format!("P{}", pad.number),
                    r#type: kiapi::common::types::ElectricalPinType::EptPassive as i32,
                    no_connect: false,
                });
                item.value = pad.encode_to_vec();
            }
        }
        for (name, value) in [("Old_Library_Generator", "old"), ("MPN", "RC0402")] {
            let mut property = footprint.value_field.clone().unwrap();
            property.id = None;
            property.name = name.to_string();
            if let Some(text) = property.text.as_mut() {
                text.id = None;
                if let Some(text) = text.text.as_mut() {
                    text.text = value.to_string();
                }
            }
            definition.items.push(copperpour_ipc::builders::pack_any(
                &property,
                "kiapi.board.types.Field",
            ));
        }
        footprint
    }

    /// `footprint`'s custom property names.
    fn custom_property_names(
        footprint: &copperpour_ipc::gen::kiapi::board::types::FootprintInstance,
    ) -> Vec<String> {
        footprint
            .definition
            .as_ref()
            .unwrap()
            .items
            .iter()
            .filter(|item| item.type_url.ends_with("kiapi.board.types.Field"))
            .map(|item| {
                copperpour_ipc::gen::kiapi::board::types::Field::decode(item.value.as_slice())
                    .unwrap()
                    .name
            })
            .collect()
    }

    /// The schematic names the stock 0603 for R1, which the board has as
    /// KiCad's R_0402. As KiCad's Update PCB does, the sync replaces it in
    /// place: same KIID, position, rotation and side, each pad's net and
    /// schematic pin by number, the new footprint's custom properties instead
    /// of the old library's, and the schematic's value. The next dry run has
    /// nothing to do.
    #[tokio::test]
    async fn a_footprint_the_schematic_names_differently_is_replaced_in_place() {
        use copperpour_ipc::gen::kiapi::board::types::FootprintInstance;
        let before = r1_with_pins_and_an_old_property();
        let served = ServedSync::holding(HeldBoard {
            footprints: vec![copperpour_ipc::builders::pack_any(
                &before,
                "kiapi.board.types.FootprintInstance",
            )],
            zones: Vec::new(),
            nets: vec!["VCC", "GND"],
        })
        .await;
        // The symbol's MPN is the board's already; the replacement drops the
        // old fields, so it has to be set again all the same.
        let netlist = r1_on(STOCK_0603).replace(
            "(tstamps \"symbol-uuid\")",
            "(tstamps \"symbol-uuid\")
      (fields (field (name \"MPN\") \"RC0402\"))",
        );

        let plan = served.dry_run(&netlist).await;
        assert_eq!(plan["status"], "ready", "{plan:#}");
        assert_eq!(plan["changes"][0]["kind"], "update", "{plan:#}");
        assert_eq!(
            plan["changes"][0]["replace_footprint"],
            serde_json::json!({ "from": "Resistor_SMD:R_0402", "to": STOCK_0603 }),
            "{plan:#}"
        );

        let applied = served.apply(&netlist).await;
        assert_eq!(applied["status"], "applied", "{applied:#}");
        assert_eq!(applied["diagnostics"], serde_json::json!([]), "{applied:#}");
        assert_eq!(served.sent("BeginCommit"), 1);
        let held =
            FootprintInstance::decode(served.held.lock().unwrap()[0].value.as_slice()).unwrap();
        let id = held.definition.as_ref().unwrap().id.clone().unwrap();
        assert_eq!(
            format!("{}:{}", id.library_nickname, id.entry_name),
            STOCK_0603
        );
        assert_eq!(held.id, before.id);
        assert_eq!(held.position, before.position);
        assert_eq!(held.orientation, before.orientation);
        assert_eq!(held.layer, before.layer);
        assert_eq!(
            held.symbol_path.as_ref().map(|path| &path.path),
            before.symbol_path.as_ref().map(|path| &path.path)
        );
        assert_eq!(field_text(&held.value_field), "10k");
        let pads = held
            .definition
            .as_ref()
            .unwrap()
            .items
            .iter()
            .filter(|item| item.type_url.ends_with("kiapi.board.types.Pad"))
            .map(|item| {
                let pad =
                    copperpour_ipc::gen::kiapi::board::types::Pad::decode(item.value.as_slice())
                        .unwrap();
                (
                    pad.number.clone(),
                    pad.net.map(|net| net.name).unwrap_or_default(),
                    pad.symbol_pin.map(|pin| pin.name).unwrap_or_default(),
                )
            })
            .collect::<Vec<_>>();
        assert_eq!(
            pads,
            vec![
                ("1".to_string(), "VCC".to_string(), "P1".to_string()),
                ("2".to_string(), "GND".to_string(), "P2".to_string()),
            ]
        );
        let properties = custom_property_names(&held);
        assert!(
            !properties.contains(&"Old_Library_Generator".to_string()),
            "{properties:?}"
        );
        assert!(
            properties.contains(&"KiLib_Generator".to_string()),
            "the stock 0603's property: {properties:?}"
        );
        assert!(
            properties.contains(&"MPN".to_string()),
            "the symbol's field, set again: {properties:?}"
        );
        assert_eq!(served.dry_run(&netlist).await["status"], "noop");
    }

    /// A library footprint in the older `fp_text reference` / `fp_text value`
    /// form, as KiCad 7 wrote them and many libraries still are: two 0603
    /// pads, a silkscreen line, and Reference and Value as texts.
    const OLDER_FORM_0603: &str = r#"(footprint "R_0603_OlderForm"
  (version 20240108)
  (generator "pcbnew")
  (layer "F.Cu")
  (descr "0603 in the fp_text form")
  (attr smd)
  (pad "1" smd roundrect (at -0.825 0) (size 0.8 0.95) (layers "F.Cu" "F.Paste" "F.Mask") (roundrect_rratio 0.25))
  (pad "2" smd roundrect (at 0.825 0) (size 0.8 0.95) (layers "F.Cu" "F.Paste" "F.Mask") (roundrect_rratio 0.25))
  (fp_line (start -0.16 -0.51) (end 0.16 -0.51) (stroke (width 0.12) (type solid)) (layer "F.SilkS"))
  (fp_text reference "REF**" (at 0 -1.43 0) (layer "F.SilkS") (effects (font (size 1 1) (thickness 0.15))))
  (fp_text value "R_0603_OlderForm" (at 0 1.43 0) (layer "F.Fab") (effects (font (size 1 1) (thickness 0.15))))
)
"#;

    /// Found live: the library reader reads Reference and Value only in the
    /// `(property …)` form, so a replacement by a footprint in the older form
    /// planned `ready` and then failed its apply. The placed text now keeps
    /// its position and style, as a refresh without a reset keeps it, and the
    /// dry run builds each replacement, so `ready` means the apply builds it.
    #[tokio::test]
    async fn a_replacement_by_an_older_form_footprint_keeps_the_placed_text() {
        use copperpour_ipc::gen::kiapi::board::types::FootprintInstance;
        let before =
            FootprintInstance::decode(schematic_backed_resistor().value.as_slice()).unwrap();
        let served = ServedSync::holding(HeldBoard {
            footprints: vec![schematic_backed_resistor()],
            zones: Vec::new(),
            nets: vec!["VCC", "GND"],
        })
        .await;
        std::fs::write(
            served
                ._temp
                .path()
                .join("Capacitor_SMD.pretty")
                .join("R_0603_OlderForm.kicad_mod"),
            OLDER_FORM_0603,
        )
        .unwrap();
        let netlist = r1_on("Capacitor_SMD:R_0603_OlderForm");

        let plan = served.dry_run(&netlist).await;
        assert_eq!(plan["status"], "ready", "{plan:#}");
        let applied = served.apply(&netlist).await;
        assert_eq!(applied["status"], "applied", "{applied:#}");
        let held =
            FootprintInstance::decode(served.held.lock().unwrap()[0].value.as_slice()).unwrap();
        let id = held.definition.as_ref().unwrap().id.clone().unwrap();
        assert_eq!(id.entry_name, "R_0603_OlderForm");
        let position = |footprint: &FootprintInstance| {
            footprint
                .reference_field
                .as_ref()
                .and_then(|field| field.text.as_ref())
                .and_then(|text| text.text.as_ref())
                .and_then(|text| text.position)
        };
        assert_eq!(position(&held), position(&before));
    }

    /// A replacement whose footprint does not resolve, or that the refresh
    /// cannot rebuild losslessly, is refused before anything is sent, with the
    /// code the sync uses for an addition's footprint.
    #[tokio::test]
    async fn a_replacement_that_cannot_be_built_is_refused_before_the_commit() {
        for (footprint_id, code) in [
            ("Nope:Missing", "footprint_library_resolution_failed"),
            (TEXAS_VQFN, "unsupported_library_footprint"),
        ] {
            let served = ServedSync::holding(HeldBoard {
                footprints: vec![schematic_backed_resistor()],
                zones: Vec::new(),
                nets: vec!["VCC", "GND"],
            })
            .await;
            let plan = served.dry_run(&r1_on(footprint_id)).await;
            assert_eq!(plan["status"], "conflict", "{plan:#}");
            assert_eq!(plan["changes"], serde_json::json!([]), "{plan:#}");
            assert_eq!(plan["diagnostics"][0]["code"], code, "{plan:#}");
            assert_eq!(plan["diagnostics"][0]["reference"], "R1", "{plan:#}");
            assert_eq!(served.sent("BeginCommit"), 0);
        }
    }

    /// KiCad's Update PCB replaces with every reset on
    /// (`board_netlist_updater.cpp` `replaceFootprint`), so the new
    /// footprint's field text and fabrication attributes replace the placed
    /// ones, where a library refresh keeps them by default (#554). R1 is given
    /// a through-hole mounting style, a position-file exclusion and a
    /// Datasheet of its own, none of which the schematic sets. The new
    /// footprint is KiCad's stock 0603 with a Datasheet property added; it
    /// replaces all three.
    #[tokio::test]
    async fn a_replacement_takes_the_new_footprints_text_and_attributes() {
        use copperpour_ipc::gen::kiapi::board::types::{FootprintInstance, FootprintMountingStyle};
        const LIBRARY_DATASHEET: &str = "https://example.com/library.pdf";
        let mut footprint =
            FootprintInstance::decode(schematic_backed_resistor().value.as_slice()).unwrap();
        let attributes = footprint.attributes.get_or_insert_with(Default::default);
        attributes.mounting_style = FootprintMountingStyle::FmsThroughHole as i32;
        attributes.exclude_from_position_files = true;
        set_field_text(
            &mut footprint.datasheet_field,
            "Datasheet",
            "https://example.com/placed.pdf",
        );
        let served = ServedSync::holding(HeldBoard {
            footprints: vec![copperpour_ipc::builders::pack_any(
                &footprint,
                "kiapi.board.types.FootprintInstance",
            )],
            zones: Vec::new(),
            nets: vec!["VCC", "GND"],
        })
        .await;
        let stock = std::str::from_utf8(include_bytes!(
            "../../tests/fixtures/c_0603_1608metric_kicad10.kicad_mod"
        ))
        .unwrap();
        let with_datasheet = stock
            .replacen(
                "(footprint \"C_0603_1608Metric\"",
                "(footprint \"C_0603_WithDatasheet\"",
                1,
            )
            .replacen(
                "\t(attr smd)",
                &format!(
                    "\t(property \"Datasheet\" \"{LIBRARY_DATASHEET}\"\n\t\t(at 0 0 0)\n\t\t\
                     (layer \"F.Fab\")\n\t\t(hide yes)\n\t\t(effects\n\t\t\t(font\n\t\t\t\t\
                     (size 1 1)\n\t\t\t\t(thickness 0.15)\n\t\t\t)\n\t\t)\n\t)\n\t(attr smd)"
                ),
                1,
            );
        assert_ne!(with_datasheet, stock);
        std::fs::write(
            served
                ._temp
                .path()
                .join("Capacitor_SMD.pretty")
                .join("C_0603_WithDatasheet.kicad_mod"),
            with_datasheet,
        )
        .unwrap();
        let netlist = r1_on("Capacitor_SMD:C_0603_WithDatasheet");
        assert_eq!(served.dry_run(&netlist).await["status"], "ready");
        let applied = served.apply(&netlist).await;
        assert_eq!(applied["status"], "applied", "{applied:#}");

        let held =
            FootprintInstance::decode(served.held.lock().unwrap()[0].value.as_slice()).unwrap();
        let attributes = held.attributes.clone().unwrap();
        assert_eq!(
            attributes.mounting_style,
            FootprintMountingStyle::FmsSmd as i32
        );
        assert!(!attributes.exclude_from_position_files);
        assert_eq!(field_text(&held.datasheet_field), LIBRARY_DATASHEET);
    }

    /// A replacement that only the dry run's build rejects: R1's two pads are
    /// both numbered 1, one on VCC and one on GND. The stock 0603 resolves,
    /// reads and keeps every connected pad number, so the preflight passes
    /// it; rebuilding the footprint refuses a logical pad that carries two
    /// nets. Without the build in the dry run this plan was `ready` and its
    /// apply failed after the commit began.
    #[tokio::test]
    async fn a_replacement_only_the_build_rejects_is_refused_before_the_commit() {
        use copperpour_ipc::gen::kiapi;
        let mut footprint = kiapi::board::types::FootprintInstance::decode(
            schematic_backed_resistor().value.as_slice(),
        )
        .unwrap();
        let mut nets = [("VCC", 1), ("GND", 2)].into_iter();
        for item in &mut footprint.definition.as_mut().unwrap().items {
            if item.type_url.ends_with("kiapi.board.types.Pad") {
                let mut pad = kiapi::board::types::Pad::decode(item.value.as_slice()).unwrap();
                let (name, code) = nets.next().expect("the capture has two pads");
                pad.number = "1".to_string();
                pad.net = Some(kiapi::board::types::Net {
                    code: Some(kiapi::board::types::NetCode { value: code }),
                    name: name.to_string(),
                });
                item.value = pad.encode_to_vec();
            }
        }
        assert_eq!(nets.next(), None, "both pads were renumbered");
        let served = ServedSync::holding(HeldBoard {
            footprints: vec![copperpour_ipc::builders::pack_any(
                &footprint,
                "kiapi.board.types.FootprintInstance",
            )],
            zones: Vec::new(),
            nets: vec!["VCC", "GND"],
        })
        .await;
        let plan = served.dry_run(&r1_on(STOCK_0603)).await;
        assert_eq!(plan["status"], "conflict", "{plan:#}");
        assert_eq!(plan["changes"], serde_json::json!([]), "{plan:#}");
        let diagnostic = &plan["diagnostics"][0];
        assert_eq!(
            diagnostic["code"], "unsupported_library_footprint",
            "{plan:#}"
        );
        assert_eq!(diagnostic["reference"], "R1", "{plan:#}");
        assert!(
            diagnostic["message"]
                .as_str()
                .is_some_and(|message| message.contains("carries multiple nets")),
            "the build's own refusal: {plan:#}"
        );
        assert_eq!(served.sent("BeginCommit"), 0);
    }

    /// R1 to be replaced by `footprint_id`, beside a C1 to be added.
    fn r1_on_beside_c1(footprint_id: &str) -> String {
        let netlist = r1_on(footprint_id);
        let closing = "(pin (num \"2\")))))))\n  (nets";
        assert!(netlist.contains(closing), "ONE_RESISTOR changed shape");
        netlist.replace(
            closing,
            &format!(
                "(pin (num \"2\"))))))\n    (comp\n      (ref \"C1\")\n      (value \"100n\")\n      \
                 (footprint \"{STOCK_0603}\")\n      (sheetpath (names \"/\") (tstamps \"/\"))\n      \
                 (tstamps \"C1-uuid\")\n      (units (unit (name \"A\") (pins (pin (num \"1\")) \
                 (pin (num \"2\")))))))\n  (nets"
            ),
        )
    }

    /// A replacement that cannot be built skips its part, not the sync: C1 is
    /// added beside it, and R1 stays as it was, footprint and value. (KiCad
    /// keeps the old footprint there and updates the rest of it; the sync
    /// leaves the whole part to a later run.)
    #[tokio::test]
    async fn a_replacement_that_cannot_be_built_skips_only_its_part() {
        use copperpour_ipc::gen::kiapi::board::types::FootprintInstance;
        let served = ServedSync::holding(HeldBoard {
            footprints: vec![schematic_backed_resistor()],
            zones: Vec::new(),
            nets: vec!["VCC", "GND"],
        })
        .await;
        let netlist = r1_on_beside_c1(TEXAS_VQFN);

        let plan = served.dry_run(&netlist).await;
        assert_eq!(plan["status"], "ready", "{plan:#}");
        assert_eq!(plan["changes_count"], 1, "{plan:#}");
        assert_eq!(plan["changes"][0]["reference"], "C1", "{plan:#}");
        assert_eq!(plan["coverage"]["footprints_updated"]["planned"], 0);
        assert_eq!(
            plan["skipped_footprints"][0]["code"], "unsupported_library_footprint",
            "{plan:#}"
        );
        assert_eq!(plan["skipped_footprints"][0]["reference"], "R1");

        served.apply(&netlist).await;
        let sent = served.sent_footprints();
        assert_eq!(sent.len(), 1);
        assert_eq!(field_text(&sent[0].reference_field), "C1");
        let held =
            FootprintInstance::decode(served.held.lock().unwrap()[0].value.as_slice()).unwrap();
        let id = held.definition.as_ref().unwrap().id.clone().unwrap();
        assert_eq!(id.entry_name, "R_0402");
        assert_eq!(field_text(&held.value_field), "1k");
    }

    /// An acknowledged routed pad net change (#38) on a part that is then
    /// skipped goes with the part: the apply adds C1, leaves R1's pad on its
    /// net, and runs no post-apply check for a change it did not make. The
    /// stand-in DRC has no report, so a check that ran would flag the apply.
    #[tokio::test]
    async fn an_acknowledged_change_on_a_skipped_part_is_neither_made_nor_checked() {
        let served = served_r1_under_the_gnd_pour().await;
        std::fs::remove_file(&served.drc_report).unwrap();
        let netlist = r1_on_beside_c1(TEXAS_VQFN).replace("(name \"GND\")", "(name \"NEW_NET\")");
        let refused = served.dry_run(&netlist).await;
        assert_eq!(refused["status"], "conflict", "{refused:#}");
        let acknowledged = acknowledging_each(&refused);
        assert_eq!(
            acknowledged.as_array().map(Vec::len),
            Some(1),
            "{refused:#}"
        );

        let (_, plan) = served
            .sync(
                &netlist,
                serde_json::json!({ "acknowledged_net_changes": acknowledged }),
            )
            .await;
        assert_eq!(plan["status"], "ready", "{plan:#}");
        assert_eq!(plan["changes_count"], 1, "{plan:#}");
        assert_eq!(plan["changes"][0]["reference"], "C1", "{plan:#}");
        assert_eq!(plan["skipped_footprints"][0]["reference"], "R1", "{plan:#}");
        // R1 keeps its pad on GND, so no pad moves the pour off it; GND has
        // left the schematic, so the pour stays with KiCad's warning (#862).
        assert_eq!(
            plan["copper_net_changes"],
            serde_json::json!([]),
            "{plan:#}"
        );
        let codes = plan["diagnostics"]
            .as_array()
            .unwrap()
            .iter()
            .map(|note| note["code"].as_str().unwrap())
            .collect::<Vec<_>>();
        assert_eq!(codes, ["zone_net_not_in_schematic"], "{plan:#}");

        let (is_error, applied) = served
            .sync(
                &netlist,
                serde_json::json!({
                    "dry_run": false,
                    "expected_plan_revision": plan["plan_revision"],
                    "acknowledged_net_changes": acknowledged,
                }),
            )
            .await;

        assert!(!is_error, "{applied:#}");
        assert_eq!(applied["status"], "applied", "{applied:#}");
        assert!(applied.get("routed_copper").is_none(), "{applied:#}");
        assert!(applied.get("drc").is_none(), "{applied:#}");
        assert_eq!(applied["skipped_footprints"], plan["skipped_footprints"]);
        assert_eq!(held_r1_pad_2(&served).as_deref(), Some("GND"));
        let sent = served.sent_footprints();
        assert_eq!(sent.len(), 1);
        assert_eq!(field_text(&sent[0].reference_field), "C1");
    }

    /// KiCad's Update PCB leaves a locked footprint's footprint as it is,
    /// with a warning, and still updates the rest of it. So does the sync:
    /// R1 keeps the R_0402, takes the schematic's value, and the plan notes
    /// the replacement it left out without refusing anything.
    #[tokio::test]
    async fn a_locked_footprint_keeps_its_footprint_and_takes_the_rest() {
        use copperpour_ipc::gen::kiapi::board::types::FootprintInstance;
        let mut locked =
            FootprintInstance::decode(schematic_backed_resistor().value.as_slice()).unwrap();
        locked.locked = copperpour_ipc::gen::kiapi::common::types::LockedState::LsLocked as i32;
        let served = ServedSync::holding(HeldBoard {
            footprints: vec![copperpour_ipc::builders::pack_any(
                &locked,
                "kiapi.board.types.FootprintInstance",
            )],
            zones: Vec::new(),
            nets: vec!["VCC", "GND"],
        })
        .await;
        let netlist = r1_on(STOCK_0603);
        let plan = served.dry_run(&netlist).await;
        assert_eq!(plan["status"], "ready", "{plan:#}");
        assert_eq!(
            plan["changes"][0].get("replace_footprint"),
            None,
            "{plan:#}"
        );
        assert_eq!(
            plan["diagnostics"][0]["code"], "footprint_locked_not_replaced",
            "{plan:#}"
        );
        assert_eq!(plan["coverage"]["conflicts"]["planned"], 0, "{plan:#}");

        served.apply(&netlist).await;
        let held =
            FootprintInstance::decode(served.held.lock().unwrap()[0].value.as_slice()).unwrap();
        let id = held.definition.as_ref().unwrap().id.clone().unwrap();
        assert_eq!(id.entry_name, "R_0402");
        assert_eq!(field_text(&held.value_field), "10k");
    }

    /// KiCad's own R1 linked to its symbol, R3 with no symbol, and MH1 with
    /// none and the board-only attribute, as the mock's board holds them
    /// (#861). R3 and MH1 are the captured board-only footprint, which KiCad
    /// sent locked and without the attribute; MH1 is given it, and R3 the
    /// lock the test asks for.
    fn held_with_footprints_without_symbols(r3_locked: bool) -> HeldBoard {
        use copperpour_ipc::gen::kiapi::common::types::LockedState;
        let mut r3 = board_only_instance("r3-kiid", "R3");
        r3.locked = if r3_locked {
            LockedState::LsLocked
        } else {
            LockedState::LsUnlocked
        } as i32;
        let mut mounting_hole = board_only_instance("mh1-kiid", "MH1");
        mounting_hole
            .attributes
            .get_or_insert_with(Default::default)
            .not_in_schematic = true;
        HeldBoard {
            footprints: vec![
                schematic_backed_resistor(),
                copperpour_ipc::builders::pack_any(&r3, "kiapi.board.types.FootprintInstance"),
                copperpour_ipc::builders::pack_any(
                    &mounting_hole,
                    "kiapi.board.types.FootprintInstance",
                ),
            ],
            zones: Vec::new(),
            nets: vec!["VCC", "GND"],
        }
    }

    /// The references of the footprints the mock's board now holds.
    fn held_references(served: &ServedSync) -> Vec<String> {
        served
            .held
            .lock()
            .unwrap()
            .iter()
            .map(|item| {
                let footprint =
                    copperpour_ipc::gen::kiapi::board::types::FootprintInstance::decode(
                        item.value.as_slice(),
                    )
                    .unwrap();
                field_text(&footprint.reference_field)
            })
            .collect()
    }

    /// The footprints a response names as kept without a symbol, as
    /// `(reference, kiid, locked)`.
    fn named_without_symbols(response: &serde_json::Value) -> Vec<(String, String, bool)> {
        response["footprints_without_symbols"]
            .as_array()
            .unwrap_or_else(|| panic!("no footprints_without_symbols: {response:#}"))
            .iter()
            .map(|footprint| {
                (
                    footprint["reference"].as_str().unwrap().to_string(),
                    footprint["kiid"].as_str().unwrap().to_string(),
                    footprint["locked"].as_bool().unwrap(),
                )
            })
            .collect()
    }

    fn r3_named(locked: bool) -> Vec<(String, String, bool)> {
        vec![("R3".to_string(), "r3-kiid".to_string(), locked)]
    }

    /// Without the option, the dry run and the apply name R3, the footprint
    /// that lost its symbol, and not MH1, which is board-only; both stay.
    #[tokio::test]
    async fn footprints_without_symbols_are_named_in_the_dry_run_and_the_apply() {
        let served = ServedSync::holding(held_with_footprints_without_symbols(false)).await;
        let netlist = r1_on(STOCK_0603);

        let plan = served.dry_run(&netlist).await;
        assert_eq!(named_without_symbols(&plan), r3_named(false), "{plan:#}");
        assert_eq!(
            plan["footprints_without_symbols"][0]["reason"],
            "symbol_removed"
        );
        assert_eq!(plan["coverage"]["board_only_preserved"]["planned"], 2);
        assert_eq!(plan["coverage"]["footprints_deleted"]["planned"], 0);

        let applied = served.apply(&netlist).await;
        assert_eq!(applied["status"], "applied", "{applied:#}");
        assert_eq!(named_without_symbols(&applied), r3_named(false));
        assert_eq!(served.sent("DeleteItems"), 0);
        assert_eq!(held_references(&served), ["R1", "R3", "MH1"]);
    }

    /// [`r1_on`] with R3 beside it, and the served schematic's R3 (KiCad's
    /// own `structural_scans_kicad10.kicad_sch`) marked Exclude from board,
    /// as `edit_schematic_component` with `on_board: false` writes it.
    fn excluding_r3(served: &ServedSync) -> String {
        let schematic = std::fs::read_to_string(&served.schematic).unwrap();
        let r3 = schematic
            .find("(reference \"R3\")")
            .expect("the fixture places R3");
        let flag = schematic[..r3]
            .rfind("(on_board yes)")
            .expect("R3 is on the board in the fixture");
        let mut excluded = schematic;
        excluded.replace_range(flag..flag + "(on_board yes)".len(), "(on_board no)");
        std::fs::write(&served.schematic, excluded).unwrap();

        let netlist = r1_on(STOCK_0603);
        let closing = "(pin (num \"2\")))))))\n  (nets";
        assert!(netlist.contains(closing), "ONE_RESISTOR changed shape");
        netlist.replace(
            closing,
            &format!(
                "(pin (num \"2\"))))))\n    (comp\n      (ref \"R3\")\n      (value \"10k\")\n      \
                 (footprint \"{STOCK_0603}\")\n      (sheetpath (names \"/\") (tstamps \"/\"))\n      \
                 (tstamps \"R3-uuid\")\n      (units (unit (name \"A\") (pins (pin (num \"1\")) \
                 (pin (num \"2\")))))))\n  (nets"
            ),
        )
    }

    /// R3 marked Exclude from board while its footprint is on the board: the
    /// sync no longer refuses, and treats the footprint as KiCad's Update PCB
    /// does, as one with no symbol (#870). The dry run names it with
    /// `excluded_from_board`; with `delete_footprints_without_symbols` it is
    /// deleted in the sync's commit.
    #[tokio::test]
    async fn an_excluded_part_s_footprint_is_named_and_deleted_on_request() {
        let served = ServedSync::holding(held_with_footprints_without_symbols(false)).await;
        let netlist = excluding_r3(&served);

        let plan = served.dry_run(&netlist).await;
        assert_ne!(plan["status"], "conflict", "{plan:#}");
        assert_eq!(
            plan["coverage"]["skipped_by_flag"]["planned"], 1,
            "{plan:#}"
        );
        assert_eq!(named_without_symbols(&plan), r3_named(false), "{plan:#}");
        assert_eq!(
            plan["footprints_without_symbols"][0]["reason"],
            "excluded_from_board"
        );

        let deleting = serde_json::json!({ "delete_footprints_without_symbols": true });
        let (_, plan) = served.sync(&netlist, deleting.clone()).await;
        assert_eq!(plan["status"], "ready", "{plan:#}");
        let deletions = plan["changes"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|change| change["kind"] == "delete")
            .cloned()
            .collect::<Vec<_>>();
        assert_eq!(deletions.len(), 1, "{plan:#}");
        assert_eq!(deletions[0]["reference"], "R3");
        assert_eq!(deletions[0]["reason"], "excluded_from_board");

        let mut apply = deleting;
        apply["dry_run"] = serde_json::json!(false);
        apply["expected_plan_revision"] = plan["plan_revision"].clone();
        let (is_error, applied) = served.sync(&netlist, apply).await;
        assert!(!is_error, "{applied:#}");
        assert_eq!(applied["status"], "applied", "{applied:#}");
        assert_eq!(applied["coverage"]["footprints_deleted"]["applied"], 1);
        assert_eq!(served.sent("DeleteItems"), 1);
        assert_eq!(held_references(&served), ["R1", "MH1"]);
    }

    /// With `delete_footprints_without_symbols`, R3 is a planned `delete`
    /// change, and the apply deletes it in the sync's one commit. MH1 stays.
    #[tokio::test]
    async fn footprints_without_symbols_are_deleted_in_the_same_commit_on_request() {
        let served = ServedSync::holding(held_with_footprints_without_symbols(false)).await;
        let netlist = r1_on(STOCK_0603);
        let deleting = serde_json::json!({ "delete_footprints_without_symbols": true });

        let (_, plan) = served.sync(&netlist, deleting.clone()).await;
        assert_eq!(plan["status"], "ready", "{plan:#}");
        let deletions = plan["changes"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|change| change["kind"] == "delete")
            .cloned()
            .collect::<Vec<_>>();
        assert_eq!(deletions.len(), 1, "{plan:#}");
        assert_eq!(deletions[0]["kiid"], "r3-kiid");
        assert_eq!(deletions[0]["reference"], "R3");
        assert_eq!(deletions[0]["reason"], "symbol_removed");
        assert_eq!(plan["coverage"]["footprints_deleted"]["planned"], 1);
        assert_eq!(plan["coverage"]["board_only_preserved"]["planned"], 1);
        assert_eq!(named_without_symbols(&plan), []);

        let mut apply = deleting;
        apply["dry_run"] = serde_json::json!(false);
        apply["expected_plan_revision"] = plan["plan_revision"].clone();
        let (is_error, applied) = served.sync(&netlist, apply).await;
        assert!(!is_error, "{applied:#}");
        assert_eq!(applied["status"], "applied", "{applied:#}");
        assert_eq!(applied["coverage"]["footprints_deleted"]["applied"], 1);
        assert_eq!(applied["diagnostics"], serde_json::json!([]), "{applied:#}");
        assert_eq!(served.sent("BeginCommit"), 1);
        assert_eq!(served.sent("DeleteItems"), 1);
        assert_eq!(held_references(&served), ["R1", "MH1"]);
    }

    /// A dry run without the option plans no deletion, so its revision does
    /// not apply with the option: the apply is stale, deletes nothing, and
    /// names R3 as kept.
    #[tokio::test]
    async fn a_plan_made_without_the_option_deletes_nothing() {
        let served = ServedSync::holding(held_with_footprints_without_symbols(false)).await;
        let netlist = r1_on(STOCK_0603);
        let plan = served.dry_run(&netlist).await;
        assert_eq!(plan["status"], "ready", "{plan:#}");

        let (_, refused) = served
            .sync(
                &netlist,
                serde_json::json!({
                    "dry_run": false,
                    "expected_plan_revision": plan["plan_revision"],
                    "delete_footprints_without_symbols": true,
                }),
            )
            .await;

        assert_eq!(refused["status"], "conflict", "{refused:#}");
        assert_eq!(refused["diagnostics"][0]["code"], "stale_plan_revision");
        assert_eq!(refused["coverage"]["footprints_deleted"]["planned"], 0);
        assert_eq!(named_without_symbols(&refused), r3_named(false));
        assert_eq!(served.sent("DeleteItems"), 0);
        assert_eq!(held_references(&served), ["R1", "R3", "MH1"]);
    }

    /// A locked R3 is kept with a `footprint_locked_not_deleted` note that
    /// refuses nothing; the rest of the plan applies.
    #[tokio::test]
    async fn a_locked_footprint_without_a_symbol_is_kept_and_noted() {
        let served = ServedSync::holding(held_with_footprints_without_symbols(true)).await;
        let netlist = r1_on(STOCK_0603);
        let deleting = serde_json::json!({ "delete_footprints_without_symbols": true });

        let (_, plan) = served.sync(&netlist, deleting.clone()).await;
        assert_eq!(plan["status"], "ready", "{plan:#}");
        assert_eq!(plan["coverage"]["footprints_deleted"]["planned"], 0);
        assert_eq!(named_without_symbols(&plan), r3_named(true));
        let notes = plan["diagnostics"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|note| note["code"] == "footprint_locked_not_deleted")
            .collect::<Vec<_>>();
        assert_eq!(notes.len(), 1, "{plan:#}");
        assert_eq!(notes[0]["reference"], "R3");
        assert_eq!(plan["coverage"]["conflicts"]["planned"], 0);

        let mut apply = deleting;
        apply["dry_run"] = serde_json::json!(false);
        apply["expected_plan_revision"] = plan["plan_revision"].clone();
        let (is_error, applied) = served.sync(&netlist, apply).await;
        assert!(!is_error, "{applied:#}");
        assert_eq!(applied["status"], "applied", "{applied:#}");
        assert_eq!(served.sent("DeleteItems"), 0);
        assert_eq!(held_references(&served), ["R1", "R3", "MH1"]);
    }

    /// KiCad before 10.0.7 answers a delete with no per-item results, so a
    /// footprint it kept would pass unnoticed. The board is read back after
    /// the commit: R3 still on it is reported, counted as kept rather than
    /// deleted, and listed with the kept footprints.
    #[tokio::test]
    async fn a_footprint_kicad_kept_after_its_deletion_is_reported() {
        let served = ServedSync::holding(held_with_footprints_without_symbols(false)).await;
        served
            .keeps_deleted
            .store(true, std::sync::atomic::Ordering::SeqCst);
        let netlist = r1_on(STOCK_0603);
        let deleting = serde_json::json!({ "delete_footprints_without_symbols": true });
        let (_, plan) = served.sync(&netlist, deleting.clone()).await;
        assert_eq!(plan["status"], "ready", "{plan:#}");

        let mut apply = deleting;
        apply["dry_run"] = serde_json::json!(false);
        apply["expected_plan_revision"] = plan["plan_revision"].clone();
        let (_, applied) = served.sync(&netlist, apply).await;

        assert_eq!(applied["status"], "applied", "{applied:#}");
        assert_eq!(served.sent("DeleteItems"), 1);
        let differs = applied["diagnostics"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|note| note["code"] == "board_readback_differs")
            .collect::<Vec<_>>();
        assert_eq!(differs.len(), 1, "{applied:#}");
        assert_eq!(differs[0]["reference"], "R3");
        assert_eq!(applied["coverage"]["footprints_deleted"]["applied"], 0);
        assert_eq!(applied["coverage"]["footprints_deleted"]["planned"], 1);
        assert_eq!(applied["coverage"]["board_only_preserved"]["applied"], 2);
        assert_eq!(named_without_symbols(&applied), r3_named(false));
    }

    /// KiCad 10.0.7+ reports each deleted item, and `delete_items_in` holds
    /// the answer to the request: the deletion goes through on that path too.
    #[tokio::test]
    async fn a_deletion_kicad_reports_item_by_item_is_applied() {
        let served = ServedSync::holding(held_with_footprints_without_symbols(false)).await;
        served
            .reports_deleted
            .store(true, std::sync::atomic::Ordering::SeqCst);
        let netlist = r1_on(STOCK_0603);
        let deleting = serde_json::json!({ "delete_footprints_without_symbols": true });
        let (_, plan) = served.sync(&netlist, deleting.clone()).await;
        assert_eq!(plan["status"], "ready", "{plan:#}");

        let mut apply = deleting;
        apply["dry_run"] = serde_json::json!(false);
        apply["expected_plan_revision"] = plan["plan_revision"].clone();
        let (is_error, applied) = served.sync(&netlist, apply).await;

        assert!(!is_error, "{applied:#}");
        assert_eq!(applied["status"], "applied", "{applied:#}");
        assert_eq!(applied["coverage"]["footprints_deleted"]["applied"], 1);
        assert_eq!(applied["diagnostics"], serde_json::json!([]), "{applied:#}");
        assert_eq!(held_references(&served), ["R1", "MH1"]);
    }

    /// A readback that fails after the commit refuses nothing: the commit
    /// stands, so the apply answers `applied` with a note for each readback
    /// it could not finish, as an error so it is not taken for a clean one.
    #[tokio::test]
    async fn a_readback_that_fails_after_the_commit_is_reported_on_the_applied_response() {
        let served = ServedSync::holding(held_with_footprints_without_symbols(false)).await;
        served
            .fails_readback
            .store(0, std::sync::atomic::Ordering::SeqCst);
        let netlist = r1_on(STOCK_0603);
        let deleting = serde_json::json!({ "delete_footprints_without_symbols": true });
        let (_, plan) = served.sync(&netlist, deleting.clone()).await;
        assert_eq!(plan["status"], "ready", "{plan:#}");

        let mut apply = deleting;
        apply["dry_run"] = serde_json::json!(false);
        apply["expected_plan_revision"] = plan["plan_revision"].clone();
        let (is_error, applied) = served.sync(&netlist, apply).await;

        assert!(is_error, "{applied:#}");
        assert_eq!(applied["status"], "applied", "{applied:#}");
        let unchecked = applied["diagnostics"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|note| note["code"] == "apply_readback_unchecked")
            .count();
        // R1's replacement and R3's deletion are each read back.
        assert_eq!(unchecked, 2, "{applied:#}");
        assert_eq!(held_references(&served), ["R1", "MH1"]);
    }

    /// Each readback after the commit flags the response on its own: with
    /// no deletion, the footprints KiCad was sent; with the first readback
    /// answered, the footprints it deleted.
    #[tokio::test]
    async fn each_readback_that_fails_after_the_commit_flags_the_response() {
        let unchecked = |applied: &serde_json::Value| {
            applied["diagnostics"]
                .as_array()
                .unwrap()
                .iter()
                .filter(|note| note["code"] == "apply_readback_unchecked")
                .count()
        };

        let sent = ServedSync::holding(held_with_footprints_without_symbols(false)).await;
        sent.fails_readback
            .store(0, std::sync::atomic::Ordering::SeqCst);
        let netlist = r1_on(STOCK_0603);
        let plan = sent.dry_run(&netlist).await;
        let (is_error, applied) = sent
            .sync(
                &netlist,
                serde_json::json!({
                    "dry_run": false,
                    "expected_plan_revision": plan["plan_revision"],
                }),
            )
            .await;
        assert!(is_error, "{applied:#}");
        assert_eq!(applied["status"], "applied", "{applied:#}");
        assert_eq!(unchecked(&applied), 1, "{applied:#}");

        // The readback of R1's replacement is answered; R3's deletion's fails.
        let deleted = ServedSync::holding(held_with_footprints_without_symbols(false)).await;
        deleted
            .fails_readback
            .store(1, std::sync::atomic::Ordering::SeqCst);
        let deleting = serde_json::json!({ "delete_footprints_without_symbols": true });
        let (_, plan) = deleted.sync(&netlist, deleting.clone()).await;
        assert_eq!(plan["status"], "ready", "{plan:#}");
        let mut apply = deleting;
        apply["dry_run"] = serde_json::json!(false);
        apply["expected_plan_revision"] = plan["plan_revision"].clone();
        let (is_error, applied) = deleted.sync(&netlist, apply).await;
        assert!(is_error, "{applied:#}");
        assert_eq!(applied["status"], "applied", "{applied:#}");
        assert_eq!(unchecked(&applied), 1, "{applied:#}");
    }

    /// An explicit `false` is the default: nothing is deleted, R3 is named.
    #[tokio::test]
    async fn delete_footprints_without_symbols_false_deletes_nothing() {
        let served = ServedSync::holding(held_with_footprints_without_symbols(false)).await;
        let (_, plan) = served
            .sync(
                &r1_on(STOCK_0603),
                serde_json::json!({ "delete_footprints_without_symbols": false }),
            )
            .await;

        assert_eq!(plan["status"], "ready", "{plan:#}");
        assert!(
            !plan["changes"]
                .as_array()
                .unwrap()
                .iter()
                .any(|change| change["kind"] == "delete"),
            "{plan:#}"
        );
        assert_eq!(plan["coverage"]["footprints_deleted"]["planned"], 0);
        assert_eq!(named_without_symbols(&plan), r3_named(false));
    }

    /// A via as KiCad lists it (the #96 repro board's first via), given
    /// `kiid` and `net` (#862).
    fn via_on(kiid: &str, net: &str) -> prost_types::Any {
        use copperpour_ipc::gen::kiapi;
        let listed = kiapi::common::commands::GetItemsResponse::decode(
            include_bytes!("../../tests/fixtures/issue_96_vias.ipc.bin").as_slice(),
        )
        .expect("the captured vias decode")
        .items;
        let mut via = kiapi::board::types::Via::decode(listed[0].value.as_slice())
            .expect("the first captured item is a via");
        via.id = Some(kiapi::common::types::Kiid {
            value: kiid.to_string(),
        });
        via.net = Some(kiapi::board::types::Net {
            code: None,
            name: net.to_string(),
        });
        copperpour_ipc::builders::pack_any(&via, "kiapi.board.types.Via")
    }

    /// The net of each via and zone the mock's board holds, by KIID.
    fn held_copper_nets(served: &ServedSync) -> BTreeMap<String, String> {
        use copperpour_ipc::gen::kiapi::board::types::Via;
        let mut nets = BTreeMap::new();
        for via in served.vias.lock().unwrap().iter() {
            let net = Via::decode(via.value.as_slice()).unwrap().net.unwrap().name;
            nets.insert(copper_kiid(via), net);
        }
        for zone in served.zones.lock().unwrap().iter() {
            let ZoneNet::Copper(net) = zone_net(zone.value.as_slice()) else {
                panic!("a copper zone");
            };
            nets.insert(copper_kiid(zone), net.name);
        }
        nets
    }

    /// The captured pour's KIID.
    fn pour_kiid() -> String {
        copper_kiid(&zone_item(GND_POUR))
    }

    /// R1 under the GND pour, as [`served_r1_under_the_gnd_pour`], with a via
    /// on GND (which the netlist renames) and a via on VCC (which it keeps).
    async fn served_with_gnd_copper() -> ServedSync {
        let served = served_r1_under_the_gnd_pour().await;
        *served.vias.lock().unwrap() = vec![via_on("via-gnd", "GND"), via_on("via-vcc", "VCC")];
        served
    }

    /// The dry run that acknowledges each routed pad change of `netlist`.
    async fn acknowledged_dry_run(
        served: &ServedSync,
        netlist: &str,
    ) -> (serde_json::Value, serde_json::Value) {
        let refused = served.dry_run(netlist).await;
        assert_eq!(refused["status"], "conflict", "{refused:#}");
        let acknowledged = acknowledging_each(&refused);
        let (_, plan) = served
            .sync(
                netlist,
                serde_json::json!({ "acknowledged_net_changes": acknowledged }),
            )
            .await;
        (plan, acknowledged)
    }

    /// The apply of `plan`, made with `acknowledged`.
    async fn apply_acknowledged(
        served: &ServedSync,
        netlist: &str,
        plan: &serde_json::Value,
        acknowledged: &serde_json::Value,
    ) -> (bool, serde_json::Value) {
        served
            .sync(
                netlist,
                serde_json::json!({
                    "dry_run": false,
                    "expected_plan_revision": plan["plan_revision"],
                    "acknowledged_net_changes": acknowledged,
                }),
            )
            .await
    }

    /// The `(kind, kiid, from, to)` of each planned copper move.
    fn copper_moves(response: &serde_json::Value) -> Vec<(String, String, String, String)> {
        response["copper_net_changes"]
            .as_array()
            .unwrap_or_else(|| panic!("no copper_net_changes: {response:#}"))
            .iter()
            .map(|change| {
                let text = |key: &str| change[key].as_str().unwrap().to_string();
                (text("kind"), text("kiid"), text("from"), text("to"))
            })
            .collect()
    }

    fn move_of(kind: &str, kiid: &str, from: &str, to: &str) -> (String, String, String, String) {
        (
            kind.to_string(),
            kiid.to_string(),
            from.to_string(),
            to.to_string(),
        )
    }

    /// #862 through `tools/call`: GND leaves the schematic as R1's pad 2
    /// moves to NEW_NET. The plan moves the via on GND and the pour with it,
    /// the via on VCC stays, and the apply sends them in the sync's commit.
    #[tokio::test]
    async fn the_vias_and_pour_of_a_renamed_net_move_in_the_same_commit() {
        let served = served_with_gnd_copper().await;
        let netlist = netlist_moving_off("GND");

        let (plan, acknowledged) = acknowledged_dry_run(&served, &netlist).await;
        assert_eq!(plan["status"], "ready", "{plan:#}");
        assert_eq!(
            copper_moves(&plan),
            [
                move_of("via", "via-gnd", "GND", "NEW_NET"),
                move_of("zone", &pour_kiid(), "GND", "NEW_NET"),
            ],
            "{plan:#}"
        );
        assert_eq!(plan["coverage"]["copper_net_changes"]["planned"], 2);

        let (is_error, applied) = apply_acknowledged(&served, &netlist, &plan, &acknowledged).await;
        assert!(!is_error, "{applied:#}");
        assert_eq!(applied["status"], "applied", "{applied:#}");
        assert_eq!(applied["coverage"]["copper_net_changes"]["applied"], 2);
        assert!(
            !applied["diagnostics"]
                .as_array()
                .unwrap()
                .iter()
                .any(|note| note["code"] == "board_readback_differs"),
            "{applied:#}"
        );
        assert_eq!(served.sent("BeginCommit"), 1);
        assert_eq!(
            held_copper_nets(&served),
            BTreeMap::from([
                ("via-gnd".to_string(), "NEW_NET".to_string()),
                ("via-vcc".to_string(), "VCC".to_string()),
                (pour_kiid(), "NEW_NET".to_string()),
            ])
        );
    }

    /// A zone first follows a pad KiCad's connectivity joins to it: the
    /// mock says the pour touches R1's pad 1, which stays on VCC, so the
    /// pour goes to VCC rather than to NEW_NET, where GND's pads went.
    /// (A pad on a GND pour is on GND in a real board; the mock only shows
    /// which source the plan takes the net from.)
    #[tokio::test]
    async fn a_zone_follows_the_pad_kicad_joins_to_it() {
        let served = served_with_gnd_copper().await;
        served
            .connected
            .lock()
            .unwrap()
            .insert(pour_kiid(), vec![r1_pad("1")]);
        let netlist = netlist_moving_off("GND");

        let (plan, _) = acknowledged_dry_run(&served, &netlist).await;

        assert_eq!(plan["status"], "ready", "{plan:#}");
        assert_eq!(
            copper_moves(&plan)[1],
            move_of("zone", &pour_kiid(), "GND", "VCC"),
            "{plan:#}"
        );
        assert!(served.sent("GetConnectedItems") >= 1);
    }

    /// KiCad before 10.0.1 cannot list a zone's pads: the zone follows the
    /// net map, and a note says why.
    #[tokio::test]
    async fn an_older_kicad_moves_a_zone_by_the_net_map_with_a_note() {
        let served = served_with_gnd_copper().await;
        served.report_kicad_version(10, 0, 0);
        served
            .connected
            .lock()
            .unwrap()
            .insert(pour_kiid(), vec![r1_pad("1")]);
        let netlist = netlist_moving_off("GND");

        let (plan, _) = acknowledged_dry_run(&served, &netlist).await;

        assert_eq!(
            copper_moves(&plan)[1],
            move_of("zone", &pour_kiid(), "GND", "NEW_NET"),
            "{plan:#}"
        );
        assert!(
            plan["diagnostics"]
                .as_array()
                .unwrap()
                .iter()
                .any(|note| note["code"] == "zone_pads_not_read"),
            "{plan:#}"
        );
        assert_eq!(served.sent("GetConnectedItems"), 0);
    }

    /// What the zones' pads say is part of the plan: a dry run that moved
    /// the pour to VCC does not apply once it would go to NEW_NET.
    #[tokio::test]
    async fn the_plan_revision_covers_the_copper_moves() {
        let served = served_with_gnd_copper().await;
        served
            .connected
            .lock()
            .unwrap()
            .insert(pour_kiid(), vec![r1_pad("1")]);
        let netlist = netlist_moving_off("GND");
        let (plan, acknowledged) = acknowledged_dry_run(&served, &netlist).await;
        assert_eq!(plan["status"], "ready", "{plan:#}");
        served.connected.lock().unwrap().clear();

        let (_, refused) = apply_acknowledged(&served, &netlist, &plan, &acknowledged).await;

        assert_eq!(refused["status"], "conflict", "{refused:#}");
        assert!(
            refused["diagnostics"]
                .as_array()
                .unwrap()
                .iter()
                .any(|note| note["code"] == "stale_plan_revision"),
            "{refused:#}"
        );
        assert_eq!(refused["copper_net_changes"], serde_json::json!([]));
        assert_eq!(held_copper_nets(&served)[&pour_kiid()], "GND");
    }

    /// The board is read back after the commit: a via or zone KiCad left on
    /// its old net is reported, naming it.
    #[tokio::test]
    async fn copper_kicad_did_not_move_is_reported() {
        let served = served_with_gnd_copper().await;
        served
            .keeps_copper
            .store(true, std::sync::atomic::Ordering::SeqCst);
        let netlist = netlist_moving_off("GND");
        let (plan, acknowledged) = acknowledged_dry_run(&served, &netlist).await;

        let (_, applied) = apply_acknowledged(&served, &netlist, &plan, &acknowledged).await;

        assert_eq!(applied["status"], "applied", "{applied:#}");
        let mut differs = applied["diagnostics"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|note| note["code"] == "board_readback_differs")
            .map(|note| note["kiid"].as_str().unwrap().to_string())
            .collect::<Vec<_>>();
        differs.sort();
        let mut expected = vec!["via-gnd".to_string(), pour_kiid()];
        expected.sort();
        assert_eq!(differs, expected, "{applied:#}");
        // The message reads as one sentence: no run of a source line's
        // indentation inside it.
        for note in applied["diagnostics"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|note| note["code"] == "board_readback_differs")
        {
            let message = note["message"].as_str().unwrap();
            assert!(!message.contains("  "), "{message:?}");
            assert!(
                message.contains("reads back on 'GND' after the sync moved it to '"),
                "{message:?}"
            );
        }
        assert_eq!(applied["coverage"]["copper_net_changes"]["planned"], 2);
        assert_eq!(applied["coverage"]["copper_net_changes"]["applied"], 0);
    }

    /// A readback of the moved copper that fails after the commit refuses
    /// nothing: the apply answers `applied`, with a note, as an error.
    #[tokio::test]
    async fn a_copper_readback_that_fails_after_the_commit_is_reported() {
        let served = served_with_gnd_copper().await;
        served
            .fails_copper_readback
            .store(true, std::sync::atomic::Ordering::SeqCst);
        let netlist = netlist_moving_off("GND");
        let (plan, acknowledged) = acknowledged_dry_run(&served, &netlist).await;
        assert_eq!(plan["status"], "ready", "{plan:#}");

        let (is_error, applied) = apply_acknowledged(&served, &netlist, &plan, &acknowledged).await;

        assert!(is_error, "{applied:#}");
        assert_eq!(applied["status"], "applied", "{applied:#}");
        assert!(
            applied["diagnostics"]
                .as_array()
                .unwrap()
                .iter()
                .any(|note| {
                    note["code"] == "apply_readback_unchecked"
                        && note["message"]
                            .as_str()
                            .unwrap()
                            .contains("the vias and zones it moved")
                }),
            "{applied:#}"
        );
        assert_eq!(
            held_copper_nets(&served)[&pour_kiid()],
            "NEW_NET",
            "the commit stands"
        );
    }

    /// R1's pad `number` as KiCad lists it inside the footprint.
    fn r1_pad(number: &str) -> prost_types::Any {
        use copperpour_ipc::gen::kiapi::board::types::{FootprintInstance, Pad};
        let r1 = FootprintInstance::decode(schematic_backed_resistor().value.as_slice()).unwrap();
        r1.definition
            .unwrap()
            .items
            .into_iter()
            .find(|child| {
                copperpour_ipc::builders::any_is(child, "kiapi.board.types.Pad")
                    && Pad::decode(child.value.as_slice()).unwrap().number == number
            })
            .unwrap_or_else(|| panic!("R1 has a pad {number}"))
    }

    /// The preflight checks a replacement's pads against the new footprint:
    /// the stock 0603 has pads 1 and 2, so a schematic connecting pad 3 is
    /// refused, and so is a board footprint whose pad 3 carries a net the
    /// replacement would drop.
    #[test]
    fn a_replacement_keeps_every_connected_pad() {
        let (_temp, board_path) = project_with_stock_footprints();
        let kicad = copperpour_ipc::IpcKiCadVersion {
            major: 10,
            minor: 0,
            patch: 8,
            full_version: "10.0.8".to_string(),
        };
        let plan_for = |component: DesignComponent, footprint: BoardFootprint| {
            let design = ExportedDesign {
                root_sheet_uuid: None,
                components: vec![component],
                skipped: Vec::new(),
                unassigned: Vec::new(),
                net_names: BTreeSet::new(),
            };
            let board = board_with(vec![footprint]);
            let plan = plan_sync("netlist", &design, &board);
            assert_eq!(plan.status, PlanStatus::Ready, "{:?}", plan.diagnostics);
            prepare_replacements(&board_path, &plan, &board, &kicad)
        };
        let mut component = resistor("R1", "/sheet/existing");
        component.footprint_id = STOCK_0603.to_string();

        let (prepared, refused) = plan_for(
            component.clone(),
            board_resistor("R1", Some("/sheet/existing")),
        );
        assert!(refused.is_empty(), "{refused:?}");
        assert!(prepared.contains_key(STOCK_0603));

        let mut connects_pad_3 = component.clone();
        connects_pad_3
            .pad_nets
            .insert("3".to_string(), "EXTRA".to_string());
        let (_, refused) = plan_for(
            connects_pad_3,
            board_resistor("R1", Some("/sheet/existing")),
        );
        assert_eq!(refused.len(), 1, "{refused:?}");
        assert_eq!(refused[0].code, "footprint_pad_missing");

        let mut has_pad_3 = board_resistor("R1", Some("/sheet/existing"));
        has_pad_3
            .pad_nets
            .insert("3".to_string(), "EXTRA".to_string());
        has_pad_3.pad_numbers.insert("3".to_string());
        let (_, refused) = plan_for(component, has_pad_3);
        assert_eq!(refused.len(), 1, "{refused:?}");
        assert_eq!(refused[0].code, "footprint_replacement_drops_connected_pad");
        assert!(
            refused[0].message.contains("pad(s) 3"),
            "{}",
            refused[0].message
        );
    }

    /// Update PCB's replacement reads the new footprint with the refresh's
    /// reader, which takes custom pads now (#679): a part moved onto the
    /// stock VQFN is prepared, and one moved onto a VQFN whose custom pads
    /// have `(clearance convexhull)` is refused by name.
    #[test]
    fn a_replacement_with_custom_pads_is_prepared_and_convexhull_refused() {
        let (_temp, board_path) = project_with_stock_footprints();
        let kicad = copperpour_ipc::IpcKiCadVersion {
            major: 10,
            minor: 0,
            patch: 8,
            full_version: "10.0.8".to_string(),
        };
        let replace_onto = |footprint_id: &str| {
            let mut component = resistor("R1", "/sheet/existing");
            component.footprint_id = footprint_id.to_string();
            let design = ExportedDesign {
                root_sheet_uuid: None,
                components: vec![component],
                skipped: Vec::new(),
                unassigned: Vec::new(),
                net_names: BTreeSet::new(),
            };
            let board = board_with(vec![board_resistor("R1", Some("/sheet/existing"))]);
            let plan = plan_sync("netlist", &design, &board);
            assert_eq!(plan.status, PlanStatus::Ready, "{:?}", plan.diagnostics);
            prepare_replacements(&board_path, &plan, &board, &kicad)
        };

        let (prepared, refused) = replace_onto(PLACEABLE_VQFN);
        assert!(refused.is_empty(), "{refused:?}");
        assert!(prepared.contains_key(PLACEABLE_VQFN));

        let (_, refused) = replace_onto(GENERIC_VQFN);
        assert_eq!(refused.len(), 1, "{refused:?}");
        assert_eq!(refused[0].code, "unsupported_library_footprint");
        assert!(
            refused[0].message.contains("custom pad '1'")
                && refused[0].message.contains("convexhull"),
            "{}",
            refused[0].message
        );
    }

    /// A footprint an earlier sync placed is linked to its symbol but has no
    /// sheet name, sheet file or filters. Its sheet alone differs from the
    /// schematic's, and that is an update, as in KiCad's Update PCB: the
    /// footprint keeps its place and takes the sheet, and the next dry run has
    /// nothing to do.
    #[tokio::test]
    async fn a_footprint_without_its_sheet_is_updated_to_it() {
        use copperpour_ipc::gen::kiapi::board::types::FootprintInstance;
        let mut footprint =
            FootprintInstance::decode(schematic_backed_resistor().value.as_slice()).unwrap();
        set_field_text(&mut footprint.value_field, "Value", "10k");
        assert_eq!(SymbolSheet::of(&footprint), SymbolSheet::default());
        let served = ServedSync::holding(HeldBoard {
            footprints: vec![copperpour_ipc::builders::pack_any(
                &footprint,
                "kiapi.board.types.FootprintInstance",
            )],
            zones: Vec::new(),
            nets: vec!["VCC", "GND"],
        })
        .await;
        let netlist = r1_at_10k();

        let plan = served.dry_run(&netlist).await;
        assert_eq!(plan["status"], "ready", "{plan:#}");
        assert_eq!(
            plan["coverage"]["footprints_updated"]["planned"], 1,
            "{plan:#}"
        );
        assert_eq!(
            plan["coverage"]["pads_reassigned"]["planned"], 0,
            "{plan:#}"
        );
        assert_eq!(plan["changes"][0]["kind"], "update", "{plan:#}");
        assert_eq!(plan["changes"][0]["sheet_name"], "/Power/", "{plan:#}");

        let applied = served.apply(&netlist).await;
        assert_eq!(applied["diagnostics"], serde_json::json!([]), "{applied:#}");
        let held =
            FootprintInstance::decode(served.held.lock().unwrap()[0].value.as_slice()).unwrap();
        // The export names no `Sheetfile` or `ki_fp_filters` for it, so both
        // are empty, as Update PCB leaves them.
        assert_eq!(
            SymbolSheet::of(&held),
            SymbolSheet {
                sheet_name: "/Power/".to_string(),
                ..SymbolSheet::default()
            }
        );
        assert_eq!(held.position, footprint.position);
        assert_eq!(served.dry_run(&netlist).await["status"], "noop");
    }

    /// #789's second increment through the served boundary. The added 0603
    /// carries its library's fields: Reference and Value on the library's
    /// layers with its visibility (Value on `F.Fab`, shown, not the hidden
    /// silkscreen field the builder gave it), and the custom
    /// `KiLib_Generator` property. Their text is the schematic's. None of the
    /// library's properties carries `(unlocked yes)`, so each is kept upright,
    /// and each is sent unlocked, as KiCad creates a field.
    #[tokio::test]
    async fn an_added_footprint_carries_its_library_fields() {
        use copperpour_ipc::gen::kiapi::board::types::BoardLayer;
        use copperpour_ipc::gen::kiapi::common::types::LockedState;

        let served = ServedSync::reading_back(Readback::AsSent).await;
        let applied = served
            .apply(&exported_netlist(&[("C1", STOCK_0603, &["1", "2"])]))
            .await;

        assert_eq!(applied["status"], "applied", "{applied:#}");
        assert_eq!(applied["diagnostics"], serde_json::json!([]), "{applied:#}");
        let sent = &served.sent_footprints()[0];
        let shown = |field: &Option<copperpour_ipc::gen::kiapi::board::types::Field>| {
            let field = field.as_ref().expect("a field");
            let text = field.text.as_ref().unwrap();
            let shown = text.text.as_ref().unwrap();
            assert!(
                shown.attributes.as_ref().unwrap().keep_upright,
                "{} is kept upright",
                field.name
            );
            (shown.text.clone(), text.layer, field.visible, text.locked)
        };
        assert_eq!(
            shown(&sent.reference_field),
            (
                "C1".to_string(),
                BoardLayer::BlFSilkS as i32,
                true,
                LockedState::LsUnlocked as i32
            )
        );
        assert_eq!(
            shown(&sent.value_field),
            (
                "part".to_string(),
                BoardLayer::BlFFab as i32,
                true,
                LockedState::LsUnlocked as i32
            )
        );
        let custom: Vec<_> = sent
            .definition
            .as_ref()
            .unwrap()
            .items
            .iter()
            .filter(|child| copperpour_ipc::builders::any_is(child, "kiapi.board.types.Field"))
            .map(|child| {
                copperpour_ipc::gen::kiapi::board::types::Field::decode(child.value.as_slice())
                    .unwrap()
            })
            .collect();
        assert_eq!(custom.len(), 1);
        assert_eq!(custom[0].name, "KiLib_Generator");
        assert_eq!(
            shown(&Some(custom[0].clone())),
            (
                "SMD_2terminal_chip_molded".to_string(),
                BoardLayer::BlFSilkS as i32,
                false,
                LockedState::LsUnlocked as i32
            )
        );
    }

    /// The readback holds the board to the fields that were sent. A board
    /// that dropped the custom property, put Value back on the hidden
    /// silkscreen field, which is what the sync used to build, and stopped
    /// keeping Reference upright, which is what the sync used to send, is
    /// reported field by field, after the commit.
    #[tokio::test]
    async fn the_readback_names_library_fields_the_board_did_not_keep() {
        let served = ServedSync::reading_back(Readback::WithoutLibraryFields).await;
        let applied = served
            .apply(&exported_netlist(&[("C1", STOCK_0603, &["1", "2"])]))
            .await;

        assert_eq!(applied["status"], "applied", "{applied:#}");
        let messages: Vec<&str> = applied["diagnostics"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|diagnostic| diagnostic["code"] == "board_readback_differs")
            .map(|diagnostic| diagnostic["message"].as_str().unwrap())
            .collect();
        assert_eq!(messages.len(), 3, "{applied:#}");
        assert!(
            messages.iter().any(|message| message.contains(
                "C1: sent field KiLib_Generator as \"SMD_2terminal_chip_molded\" on F.SilkS, hidden, kept upright, board now has none"
            )),
            "{messages:#?}"
        );
        assert!(
            messages.iter().any(|message| message.contains(
                "C1: sent field Value as \"part\" on F.Fab, visible, kept upright, board now has \"part\" on F.SilkS, hidden, kept upright"
            )),
            "{messages:#?}"
        );
        assert!(
            messages.iter().any(|message| message.contains(
                "C1: sent field Reference as \"C1\" on F.SilkS, visible, kept upright, board now has \"C1\" on F.SilkS, visible, not kept upright"
            )),
            "{messages:#?}"
        );
    }

    /// #788 through the served boundary: the dry run lists the symbol's fields
    /// and the added 0603 carries them. `KiLib_Generator`, which the library
    /// has too, keeps the library's layer and visibility; `LCSC`, Datasheet and
    /// Description arrive hidden on F.Fab; Footprint is not carried.
    #[tokio::test]
    async fn an_added_footprint_carries_its_symbols_fields_over_its_librarys() {
        use copperpour_ipc::gen::kiapi::board::types::{BoardLayer, Field};

        let served = ServedSync::reading_back(Readback::AsSent).await;
        let netlist = exported_netlist(&[("C1", STOCK_0603, &["1", "2"])]).replacen(
            "(value \"part\")",
            "(value \"part\")\n      (fields\n        (field (name \"LCSC\") \"C788001\")\n        (field (name \"KiLib_Generator\") \"from the symbol\")\n        (field (name \"Footprint\") \"Capacitor_SMD:C_0603_1608Metric\")\n        (field (name \"Datasheet\"))\n        (field (name \"Description\") \"Unpolarized capacitor\"))",
            1,
        );

        let plan = served.dry_run(&netlist).await;
        assert_eq!(
            plan["changes"][0]["fields"],
            serde_json::json!([
                {"name": "LCSC", "value": "C788001"},
                {"name": "KiLib_Generator", "value": "from the symbol"},
                {"name": "Datasheet", "value": ""},
                {"name": "Description", "value": "Unpolarized capacitor"}
            ]),
            "{plan:#}"
        );

        let applied = served.apply(&netlist).await;
        assert_eq!(applied["status"], "applied", "{applied:#}");
        assert_eq!(applied["diagnostics"], serde_json::json!([]), "{applied:#}");
        let sent = &served.sent_footprints()[0];
        let shown = |field: &Field| {
            let text = field.text.as_ref().unwrap();
            (
                text.text.as_ref().unwrap().text.clone(),
                text.layer,
                field.visible,
            )
        };
        let hidden_on_fab = |value: &str| (value.to_string(), BoardLayer::BlFFab as i32, false);
        assert_eq!(
            shown(sent.datasheet_field.as_ref().unwrap()),
            hidden_on_fab("")
        );
        assert_eq!(
            shown(sent.description_field.as_ref().unwrap()),
            hidden_on_fab("Unpolarized capacitor")
        );
        let custom = field_items(sent);
        assert_eq!(
            custom.keys().collect::<Vec<_>>(),
            ["KiLib_Generator", "LCSC"]
        );
        assert_eq!(shown(&custom["LCSC"]), hidden_on_fab("C788001"));
        // The library's own field, as #820's reader gives it, with the
        // symbol's text.
        let library = prepare_footprint_source(include_str!(
            "../../tests/fixtures/c_0603_1608metric_kicad10.kicad_mod"
        ))
        .unwrap()
        .library_fields
        .unwrap()
        .custom
        .into_iter()
        .find(|field| field.name == "KiLib_Generator")
        .expect("the library's generator field");
        let (text, layer, visible) = shown(&custom["KiLib_Generator"]);
        assert_eq!(text, "from the symbol");
        assert_eq!(
            (layer, visible),
            (library.text.as_ref().unwrap().layer, library.visible)
        );
    }

    /// A footprint whose properties the reader refuses is still placed, with
    /// the fields every footprint gets, and the plan says which footprint and
    /// part and why. Presentation metadata never refuses a part (#789).
    #[tokio::test]
    async fn a_footprint_whose_properties_cannot_be_read_is_placed_and_reported() {
        let served = ServedSync::reading_back(Readback::AsSent).await;
        // The stock 0603 with its Reference property written twice, the shape
        // of the 15 stock footprints the reader refuses.
        let library = served.board.parent().unwrap().join("Capacitor_SMD.pretty");
        let source = include_str!("../../tests/fixtures/c_0603_1608metric_kicad10.kicad_mod");
        let twice = source.replacen(
            "\t(property \"KiLib_Generator\"",
            "\t(property \"Reference\" \"REF**\"\n\t\t(at 0 0 0)\n\t\t(layer \"F.Fab\")\n\t\t(effects (font (size 1 1) (thickness 0.15)))\n\t)\n\t(property \"KiLib_Generator\"",
            1,
        );
        assert_ne!(twice, source);
        std::fs::write(library.join("C_0603_TwoReferences.kicad_mod"), twice).unwrap();
        let footprint_id = "Capacitor_SMD:C_0603_TwoReferences";

        let plan = served
            .dry_run(&exported_netlist(&[("C1", footprint_id, &["1", "2"])]))
            .await;

        assert_eq!(plan["status"], "ready", "{plan:#}");
        assert_eq!(plan["changes"].as_array().unwrap().len(), 1, "{plan:#}");
        let diagnostics = plan["diagnostics"].as_array().unwrap();
        assert_eq!(diagnostics.len(), 1, "{plan:#}");
        assert_eq!(diagnostics[0]["code"], "library_fields_not_carried");
        assert_eq!(diagnostics[0]["footprint_id"], footprint_id);
        assert_eq!(diagnostics[0]["references"], serde_json::json!(["C1"]));
        let message = diagnostics[0]["message"].as_str().unwrap();
        assert!(message.contains("more than once"), "{message}");
    }

    // ─── #149: pad fabrication properties ────────────────────────────────────

    /// Each pad's number and fabrication property, sorted.
    fn pad_properties_of(
        footprint: &copperpour_ipc::gen::kiapi::board::types::FootprintInstance,
    ) -> Vec<(String, i32)> {
        let mut pads = footprint
            .definition
            .as_ref()
            .unwrap()
            .items
            .iter()
            .filter(|item| copperpour_ipc::builders::any_is(item, "kiapi.board.types.Pad"))
            .map(|item| {
                let pad =
                    copperpour_ipc::gen::kiapi::board::types::Pad::decode(item.value.as_slice())
                        .unwrap();
                (pad.number, pad.fab_property)
            })
            .collect::<Vec<_>>();
        pads.sort();
        pads
    }

    /// On KiCad 10.0.7+, an added footprint's pads go out with the library's
    /// fabrication properties, as KiCad 10.0.8 itself reports the same stock
    /// footprint placed from its library: eight `PFP_HEATSINK` pads numbered
    /// 15 and `PFP_NONE` on the other 25.
    #[tokio::test]
    async fn an_added_footprint_carries_its_pad_fabrication_properties() {
        use copperpour_ipc::gen::kiapi::board::types::{FootprintInstance, PadFabricationProperty};

        let served = ServedSync::reading_back(Readback::AsSent).await;
        let netlist = exported_netlist(&[("U1", THERMAL_DFN, &["1", "15"])]);

        let applied = served.apply(&netlist).await;

        assert_eq!(applied["status"], "applied", "{applied:#}");
        assert_eq!(applied["diagnostics"], serde_json::json!([]), "{applied:#}");
        let kicad = FootprintInstance::decode(
            include_bytes!("../../tests/fixtures/issue_283_u1.ipc.bin").as_slice(),
        )
        .unwrap();
        let expected = pad_properties_of(&kicad);
        let heatsink = PadFabricationProperty::PfpHeatsink as i32;
        assert_eq!(
            expected
                .iter()
                .filter(|(_, property)| *property == heatsink)
                .count(),
            8
        );
        assert_eq!(pad_properties_of(&served.sent_footprints()[0]), expected);
        // Asked once per call: the dry run and the apply.
        assert_eq!(served.sent("GetVersion"), 2);
    }

    /// KiCad before 10.0.7 cannot take the property over its API, so the
    /// footprint is placed without it, and the dry run and the apply both
    /// name the footprint, the part, the pads and the KiCad version.
    #[tokio::test]
    async fn pad_fabrication_properties_an_older_kicad_cannot_take_are_reported() {
        let served = ServedSync::reading_back(Readback::AsSent).await;
        served.report_kicad_version(10, 0, 6);
        let netlist = exported_netlist(&[("U1", THERMAL_DFN, &["1", "15"])]);

        let plan = served.dry_run(&netlist).await;
        assert_eq!(plan["status"], "ready", "{plan:#}");
        let applied = served.apply(&netlist).await;

        for response in [&plan, &applied] {
            let diagnostics = response["diagnostics"].as_array().unwrap();
            assert_eq!(diagnostics.len(), 1, "{response:#}");
            assert_eq!(diagnostics[0]["code"], "not_carried");
            assert_eq!(diagnostics[0]["footprint_id"], THERMAL_DFN);
            assert_eq!(diagnostics[0]["references"], serde_json::json!(["U1"]));
            let message = diagnostics[0]["message"].as_str().unwrap();
            for part in ["pad 15: heatsink (8 pads)", "10.0.7", "10.0.6", "U1"] {
                assert!(message.contains(part), "{part} missing from: {message}");
            }
        }
        assert_eq!(applied["status"], "applied", "{applied:#}");
        // Sent as before: KiCad 10.0.6 has no field to read.
        assert!(pad_properties_of(&served.sent_footprints()[0])
            .iter()
            .all(|(_, property)| *property == 0));
    }

    /// KiCad 10.0.7+ reports `PFP_NONE` for a pad sent without a property,
    /// which is no difference: with and without pad properties.
    #[tokio::test]
    async fn a_pad_reported_without_a_property_is_no_readback_difference() {
        for (reference, footprint_id, pads) in [
            ("C1", STOCK_0603, ["1", "2"]),
            ("U1", THERMAL_DFN, ["1", "15"]),
        ] {
            let served = ServedSync::reading_back(Readback::UnknownPadPropertiesAsNone).await;
            let applied = served
                .apply(&exported_netlist(&[(reference, footprint_id, &pads)]))
                .await;
            assert_eq!(applied["status"], "applied", "{applied:#}");
            assert_eq!(applied["diagnostics"], serde_json::json!([]), "{applied:#}");
        }
    }

    /// A plan made on one side of KiCad 10.0.7 is stale on the other: the
    /// apply would send something the dry run didn't describe.
    #[tokio::test]
    async fn a_plan_is_stale_across_the_kicad_version_that_carries_pad_properties() {
        let served = ServedSync::reading_back(Readback::AsSent).await;
        let netlist = exported_netlist(&[("U1", THERMAL_DFN, &["1", "15"])]);
        for (planned_on, applied_on) in [(6, 8), (8, 6)] {
            served.report_kicad_version(10, 0, planned_on);
            let plan = served.dry_run(&netlist).await;
            assert_eq!(plan["status"], "ready", "{plan:#}");
            served.report_kicad_version(10, 0, applied_on);
            let (_, applied) = served
                .sync(
                    &netlist,
                    serde_json::json!({
                        "dry_run": false,
                        "expected_plan_revision": plan["plan_revision"],
                    }),
                )
                .await;
            assert_eq!(applied["status"], "conflict", "{applied:#}");
            let codes = applied["diagnostics"]
                .as_array()
                .unwrap()
                .iter()
                .map(|diagnostic| diagnostic["code"].as_str().unwrap())
                .collect::<Vec<_>>();
            assert!(codes.contains(&"stale_plan_revision"), "{applied:#}");
        }
        assert!(served.sent_footprints().is_empty());
    }

    /// The readback names only the pads that differ, so a BGA's one lost
    /// pad isn't hidden behind the same first entries of two long lists.
    #[test]
    fn the_readback_names_only_the_pads_whose_property_differs() {
        let bga = |number: &str| (number.to_string(), PadFabricationProperty::Bga);
        let mut sent = (1..=20).map(|n| bga(&format!("A{n}"))).collect::<Vec<_>>();
        sent.push(bga("A1"));
        sent.sort();
        let kept = sent
            .iter()
            .filter(|(number, _)| number != "A17")
            .cloned()
            .collect::<Vec<_>>();
        assert_eq!(
            sorted_difference(&sent, &kept),
            [("A17", PadFabricationProperty::Bga)]
        );
        assert!(sorted_difference(&kept, &sent).is_empty());
        // A repeated number counts: one of the two A1 pads lost it.
        let one_a1 = sent[1..].to_vec();
        assert_eq!(sent[0].0, "A1");
        assert_eq!(
            sorted_difference(&sent, &one_a1),
            [("A1", PadFabricationProperty::Bga)]
        );
    }

    /// A sync whose footprints have no pad property never asks KiCad its
    /// version, so it sends what it sent before #149.
    #[tokio::test]
    async fn a_footprint_without_pad_properties_never_asks_the_kicad_version() {
        let served = ServedSync::reading_back(Readback::AsSent).await;

        let applied = served
            .apply(&exported_netlist(&[("C1", STOCK_0603, &["1", "2"])]))
            .await;

        assert_eq!(applied["status"], "applied", "{applied:#}");
        assert_eq!(served.sent("GetVersion"), 0);
    }

    /// A board that comes back without the pad properties that were sent is
    /// reported, as a lost attribute or model is.
    #[tokio::test]
    async fn pad_fabrication_properties_the_board_lost_are_reported() {
        let served = ServedSync::reading_back(Readback::WithoutPadProperties).await;

        let applied = served
            .apply(&exported_netlist(&[("U1", THERMAL_DFN, &["1", "15"])]))
            .await;

        assert_eq!(applied["status"], "applied", "{applied:#}");
        let diagnostics = applied["diagnostics"].as_array().unwrap();
        assert_eq!(diagnostics.len(), 1, "{applied:#}");
        assert_eq!(diagnostics[0]["code"], "board_readback_differs");
        let message = diagnostics[0]["message"].as_str().unwrap();
        assert!(
            message.contains(
                "U1: pad fabrication properties sent but not on the board \
                 [pad 15: heatsink (8 pads)], on the board but not sent []"
            ),
            "{message}"
        );
    }

    /// The apply reports the same `staging` evidence as the dry run, owed
    /// from #799: both read it off the plan made against the snapshot.
    #[tokio::test]
    async fn the_apply_reports_staging_as_the_dry_run_does() {
        let served = ServedSync::refusing(None, kicad_10_refusals()).await;
        let netlist = exported_netlist(&[("C1", STOCK_0603, &["1", "2"])]);

        let planned = served.dry_run(&netlist).await;
        let applied = served.apply(&netlist).await;

        assert_eq!(applied["status"], "applied", "{applied:#}");
        let expected = serde_json::json!({
            "basis": "no_measured_geometry",
            "unavailable_item_classes": [refused_class("tables"), refused_class("generators")],
        });
        assert_eq!(planned["staging"], expected, "{planned:#}");
        assert_eq!(applied["staging"], expected, "{applied:#}");
    }

    /// The schematic decides DNP, over whatever the library footprint says,
    /// as KiCad's own Update PCB from Schematic does. The library data is
    /// applied first and the schematic's fields second, so the order is what
    /// this pins: both directions, on the KiCad-written 0603.
    #[test]
    fn the_schematic_decides_dnp_over_the_library() {
        use copperpour_ipc::gen::kiapi;

        let mut part = prepare_footprint_source(include_str!(
            "../../tests/fixtures/c_0603_1608metric_kicad10.kicad_mod"
        ))
        .unwrap();
        let addition = |dnp: bool| PlannedChange::Add {
            reference: "C1".into(),
            value: "100n".into(),
            footprint_id: STOCK_0603.into(),
            symbol_path: "/c1-uuid".into(),
            dnp,
            exclude_from_bom: false,
            pad_nets: BTreeMap::new(),
            unconnected_pins: BTreeSet::new(),
            position: Point { x: 10.0, y: 10.0 },
            fields: Vec::new(),
            sheet: SymbolSheet::default(),
        };
        let built_dnp = |part: &PreparedFootprint, dnp: bool| {
            let item = build_added_footprint(
                &addition(dnp),
                part,
                &BTreeMap::new(),
                CarriedByKiCad::default(),
            )
            .unwrap();
            let footprint =
                kiapi::board::types::FootprintInstance::decode(item.value.as_slice()).unwrap();
            let attributes = footprint.attributes.unwrap();
            assert_eq!(
                attributes.mounting_style(),
                kiapi::board::types::FootprintMountingStyle::FmsSmd,
                "the library's attributes are still applied"
            );
            attributes.do_not_populate
        };

        // The stock 0603 is not DNP in the library; the schematic says it is.
        assert!(!part.attributes.do_not_populate);
        assert!(built_dnp(&part, true));

        // A library footprint marked DNP; the schematic says it is fitted.
        part.attributes.do_not_populate = true;
        assert!(!built_dnp(&part, false));
    }

    /// A visible custom property travels as its typed field only. Drawing it as
    /// plain text as well, which the sync did when it carried no fields, would
    /// show it twice. The text drawing stays the fallback for a footprint whose
    /// properties cannot be read.
    #[test]
    fn a_visible_custom_property_is_carried_once() {
        let drawn_text = |part: &PreparedFootprint| {
            part.graphics
                .iter()
                .filter(|graphic| {
                    matches!(graphic, copperpour_ipc::IpcGraphicDefinition::Text { text, .. }
                        if text == "SMD_2terminal_chip_molded")
                })
                .count()
        };
        // The 0603's KiLib_Generator property, shown instead of hidden.
        let source = include_str!("../../tests/fixtures/c_0603_1608metric_kicad10.kicad_mod");
        let visible = source.replacen(
            "\t\t(layer \"F.SilkS\")\n\t\t(hide yes)",
            "\t\t(layer \"F.SilkS\")",
            1,
        );
        assert_ne!(visible, source);

        let part = prepare_footprint_source(&visible).unwrap();
        let fields = part.library_fields.as_ref().expect("readable properties");
        assert!(fields.custom[0].visible);
        assert_eq!(drawn_text(&part), 0, "carried as a field, not drawn too");

        // Unreadable properties: nothing is carried, so it is drawn as before.
        let twice = visible.replacen(
            "\t(property \"KiLib_Generator\"",
            "\t(property \"Value\" \"again\"\n\t\t(at 0 0 0)\n\t\t(layer \"F.Fab\")\n\t\t(effects (font (size 1 1) (thickness 0.15)))\n\t)\n\t(property \"KiLib_Generator\"",
            1,
        );
        let fallback = prepare_footprint_source(&twice).unwrap();
        assert!(fallback.library_fields.is_err());
        assert_eq!(drawn_text(&fallback), 1, "drawn when it cannot be carried");
    }

    /// A model the shared reader refuses refuses the footprint, under the code
    /// the sync already uses for a footprint it cannot place, rather than
    /// placing it without the model.
    #[test]
    fn a_library_model_the_reader_refuses_refuses_the_footprint() {
        let source = include_str!("../../tests/fixtures/c_0603_1608metric_kicad10.kicad_mod")
            .replacen("(offset", "(origin", 1);

        let refusal = prepare_footprint_source(&source).unwrap_err();

        assert_eq!(refusal.0, "unsupported_library_footprint");
        assert!(
            refusal.1.contains("3D model clause 'origin'"),
            "{}",
            refusal.1
        );
    }

    // ─── #836: refuse an update that would drop what a footprint owns ───────

    /// A board KiCad wrote: U1 owns a group, a point and a variant, R1 owns
    /// none of them, and a board-level group holds both.
    /// Provenance in `tests/fixtures/footprint_children_kicad10.README.md`.
    const FOOTPRINT_CHILDREN: &str =
        include_str!("../../tests/fixtures/footprint_children_kicad10.kicad_pcb");
    const OWNS_ALL_THREE: &str = "c7901245-d0c4-476f-8215-85f1183d04de";
    const OWNS_NONE: &str = "20a44d6b-d6bb-4ec1-ac0c-f1d3d7e20db2";
    const REFUSED_FOR: &str = "R1 owns 1 group, 1 point and variant 'NoTimer'";

    #[test]
    fn what_a_footprint_owns_is_read_from_the_board_kicad_wrote() {
        let owned = not_carried_by_kiid(FOOTPRINT_CHILDREN).unwrap();

        assert_eq!(
            owned[OWNS_ALL_THREE],
            NotCarried {
                groups: 1,
                points: 1,
                variants: vec!["NoTimer".to_string()],
            }
        );
        assert_eq!(owned[OWNS_NONE], NotCarried::default());
    }

    /// Whichever footprint comes last, neither hides what the other owns.
    #[test]
    fn a_kiid_written_twice_owns_what_both_footprints_own() {
        let owned = not_carried_by_kiid(
            "(kicad_pcb\n\
             (footprint \"A\" (uuid \"twice\") (group \"\" (uuid \"g\") (members \"m\")))\n\
             (footprint \"B\" (uuid \"twice\") (point (at 0 0) (uuid \"p\"))))",
        )
        .unwrap();

        assert_eq!(
            owned["twice"],
            NotCarried {
                groups: 1,
                points: 1,
                variants: Vec::new(),
            }
        );
    }

    /// The captured R1 as `reference` at `value` under `kiid`, linked to the
    /// symbol `symbol` on `/Power/`, with `sheet_name` as its sheet name.
    fn captured_resistor(
        kiid: &str,
        reference: &str,
        symbol: &str,
        value: &str,
        sheet_name: &str,
    ) -> prost_types::Any {
        use copperpour_ipc::gen::kiapi;

        let mut footprint = kiapi::board::types::FootprintInstance::decode(
            include_bytes!("../../tests/fixtures/issue_474_r1.ipc.bin").as_slice(),
        )
        .unwrap();
        footprint.id = Some(kiapi::common::types::Kiid {
            value: kiid.to_string(),
        });
        footprint.symbol_path = Some(kiapi::common::types::SheetPath {
            path: ["sheet-uuid", symbol]
                .into_iter()
                .map(|value| kiapi::common::types::Kiid {
                    value: value.to_string(),
                })
                .collect(),
            path_human_readable: "/Power/".to_string(),
        });
        footprint.symbol_sheet_name = sheet_name.to_string();
        set_field_text(&mut footprint.reference_field, "Reference", reference);
        set_field_text(&mut footprint.value_field, "Value", value);
        copperpour_ipc::builders::pack_any(&footprint, "kiapi.board.types.FootprintInstance")
    }

    /// KiCad holding `footprints`, which answers `SaveDocumentToString` with
    /// [`FOOTPRINT_CHILDREN`].
    async fn served_children(footprints: Vec<prost_types::Any>) -> ServedSync {
        let served = ServedSync::holding(HeldBoard {
            footprints,
            zones: Vec::new(),
            nets: vec!["VCC", "GND"],
        })
        .await;
        served.hold_board_text(FOOTPRINT_CHILDREN);
        served
    }

    /// KiCad holding the captured R1 at `value` under `kiid`, a KIID of
    /// [`FOOTPRINT_CHILDREN`], with the sheet name KiCad's Update PCB gives it
    /// (#237).
    async fn served_r1(kiid: &str, value: &str) -> ServedSync {
        served_children(vec![captured_resistor(
            kiid,
            "R1",
            "symbol-uuid",
            value,
            "/Power/",
        )])
        .await
    }

    /// The schematic's R1, at 10k, on the captured footprint and its nets.
    fn r1_at_10k() -> String {
        ONE_RESISTOR
            .replace("Resistor_SMD:R_0603_1608Metric", "Resistor_SMD:R_0402")
            .replace("/Power/VCC", "VCC")
    }

    /// A dry run, then the apply of its plan revision, whatever its status.
    async fn dry_run_then_apply(served: &ServedSync) -> (serde_json::Value, serde_json::Value) {
        let (_, plan) = served.dry_run_result(&r1_at_10k()).await;
        let (_, applied) = served
            .call(serde_json::json!({
                "schematic": served.schematic.to_string_lossy(),
                "board": served.board.to_string_lossy(),
                "dry_run": false,
                "expected_plan_revision": plan["plan_revision"],
            }))
            .await;
        (plan, applied)
    }

    fn assert_refused(body: &serde_json::Value, because: &str) {
        assert_eq!(body["status"], "conflict", "{body:#}");
        assert_eq!(body["changes"], serde_json::json!([]));
        assert_eq!(body["coverage"]["footprints_updated"]["planned"], 0);
        let diagnostic = &body["diagnostics"][0];
        assert_eq!(diagnostic["code"], "footprint_children_not_carried");
        assert_eq!(diagnostic["reference"], "R1");
        assert_eq!(diagnostic["footprint_id"], "Resistor_SMD:R_0402");
        let message = diagnostic["message"].as_str().unwrap();
        assert!(message.starts_with(because), "{message}");
    }

    #[tokio::test]
    async fn an_update_that_would_drop_what_a_footprint_owns_is_refused_before_the_commit() {
        let served = served_r1(OWNS_ALL_THREE, "1k").await;

        let (plan, applied) = dry_run_then_apply(&served).await;

        assert_refused(&plan, REFUSED_FOR);
        assert_refused(&applied, REFUSED_FOR);
        assert_eq!(served.sent("BeginCommit"), 0, "refused before the commit");
        assert_eq!(served.sent("UpdateItems"), 0);
    }

    /// The control: the same update of a footprint that owns none of them,
    /// read from the same board, is applied.
    #[tokio::test]
    async fn a_footprint_that_owns_none_of_them_is_still_updated() {
        use copperpour_ipc::gen::kiapi;

        let served = served_r1(OWNS_NONE, "1k").await;

        let (plan, applied) = dry_run_then_apply(&served).await;

        assert_eq!(plan["status"], "ready", "{plan:#}");
        assert_eq!(applied["status"], "applied", "{applied:#}");
        assert_eq!(applied["coverage"]["footprints_updated"]["applied"], 1);
        let held = served.held.lock().unwrap();
        let instance =
            kiapi::board::types::FootprintInstance::decode(held[0].value.as_slice()).unwrap();
        let r1 = board_footprint_from_instance(&instance).unwrap();
        assert_eq!((r1.kiid.as_str(), r1.value.as_str()), (OWNS_NONE, "10k"));
    }

    /// The apply reads the board again: a footprint that gains a group, a
    /// point and a variant after a `ready` dry run is refused then.
    #[tokio::test]
    async fn a_footprint_that_gains_them_after_the_dry_run_is_refused_at_apply() {
        let served = served_r1(OWNS_NONE, "1k").await;
        let (_, plan) = served.dry_run_result(&r1_at_10k()).await;
        assert_eq!(plan["status"], "ready", "{plan:#}");

        // The same board with the two footprints' KIIDs swapped.
        served.hold_board_text(
            &FOOTPRINT_CHILDREN
                .replace(OWNS_NONE, "\u{0}")
                .replace(OWNS_ALL_THREE, OWNS_NONE)
                .replace('\u{0}', OWNS_ALL_THREE),
        );
        let (_, applied) = served
            .call(serde_json::json!({
                "schematic": served.schematic.to_string_lossy(),
                "board": served.board.to_string_lossy(),
                "dry_run": false,
                "expected_plan_revision": plan["plan_revision"],
            }))
            .await;

        assert_refused(&applied, REFUSED_FOR);
        assert_eq!(served.sent("UpdateItems"), 0);
    }

    /// The board text is read for updates only: R1 already holds 10k.
    #[tokio::test]
    async fn a_plan_without_updates_does_not_read_the_board() {
        let served = served_r1(OWNS_ALL_THREE, "10k").await;

        let (_, plan) = served.dry_run_result(&r1_at_10k()).await;

        assert_eq!(plan["status"], "noop", "{plan:#}");
        assert_eq!(served.sent("SaveDocumentToString"), 0);
    }

    /// A footprint the board text does not hold is not taken to own nothing.
    #[tokio::test]
    async fn a_footprint_missing_from_the_board_kicad_wrote_is_refused() {
        let served = served_r1("00000000-0000-0000-0000-000000000836", "1k").await;

        let (plan, _) = dry_run_then_apply(&served).await;

        assert_refused(
            &plan,
            "R1 (00000000-0000-0000-0000-000000000836) is not in the board",
        );
        assert_eq!(served.sent("UpdateItems"), 0);
    }

    /// Nor is a footprint on a board text that cannot be read.
    #[tokio::test]
    async fn a_board_text_that_does_not_parse_refuses_each_update() {
        let served = served_r1(OWNS_NONE, "1k").await;
        served.hold_board_text("(kicad_pcb (footprint");

        let (plan, _) = dry_run_then_apply(&served).await;

        assert_refused(&plan, "what an update of R1 would drop is unknown");
        assert_eq!(served.sent("UpdateItems"), 0);
    }

    /// A board text that cannot be read, or that lacks a footprint the plan
    /// updates, says nothing about the part: the plan is refused whole, as
    /// before parts were skipped, and C1's addition beside R1's update is not
    /// planned alone.
    #[tokio::test]
    async fn an_unknown_board_text_refuses_the_whole_plan_not_each_update() {
        for (board_text, because) in [
            (
                "(kicad_pcb (footprint",
                "what an update of R1 would drop is unknown",
            ),
            ("(kicad_pcb (version 20241229))", "R1 ("),
        ] {
            let served = ServedSync::holding(HeldBoard {
                footprints: vec![schematic_backed_resistor()],
                zones: Vec::new(),
                nets: vec!["VCC", "GND"],
            })
            .await;
            served.hold_board_text(board_text);

            let plan = served
                .dry_run(&r1_on_beside_c1("Resistor_SMD:R_0402"))
                .await;

            assert_refused(&plan, because);
            assert_eq!(
                plan["skipped_footprints"],
                serde_json::json!([]),
                "{plan:#}"
            );
            assert_eq!(
                plan["coverage"]["footprints_added"]["planned"], 0,
                "{plan:#}"
            );
        }
    }

    /// R1 and R2 at 10k on `/Power/`, each on the captured footprint's nets.
    const TWO_RESISTORS: &str = r#"
(export
  (components
    (comp
      (ref "R1")
      (value "10k")
      (footprint "Resistor_SMD:R_0402")
      (sheetpath (names "/Power/") (tstamps "/sheet-uuid/"))
      (tstamps "symbol-uuid")
      (units (unit (name "A") (pins (pin (num "1")) (pin (num "2"))))))
    (comp
      (ref "R2")
      (value "10k")
      (footprint "Resistor_SMD:R_0402")
      (sheetpath (names "/Power/") (tstamps "/sheet-uuid/"))
      (tstamps "symbol-uuid-2")
      (units (unit (name "A") (pins (pin (num "1")) (pin (num "2")))))))
  (nets
    (net (code "1") (name "VCC") (class "Default")
      (node (ref "R1") (pin "1") (pintype "passive"))
      (node (ref "R2") (pin "1") (pintype "passive")))
    (net (code "2") (name "GND") (class "Default")
      (node (ref "R1") (pin "2") (pintype "passive"))
      (node (ref "R2") (pin "2") (pintype "passive")))))
"#;

    /// #237's first sync of a board an earlier sync built: R1 owns a group,
    /// a point and a variant and lacks only its sheet, so it leaves the plan
    /// with a diagnostic that refuses nothing, and R2's value change applies.
    #[tokio::test]
    async fn a_footprint_that_owns_them_and_lacks_only_its_sheet_is_left_out() {
        use copperpour_ipc::gen::kiapi::board::types::FootprintInstance;

        let r1 = captured_resistor(OWNS_ALL_THREE, "R1", "symbol-uuid", "10k", "");
        let r2 = captured_resistor(OWNS_NONE, "R2", "symbol-uuid-2", "1k", "/Power/");
        let served = served_children(vec![r1.clone(), r2]).await;

        let plan = served.dry_run(TWO_RESISTORS).await;

        assert_eq!(plan["status"], "ready", "{plan:#}");
        assert_eq!(plan["coverage"]["footprints_updated"]["planned"], 1);
        let changes = plan["changes"].as_array().unwrap();
        assert_eq!(changes.len(), 1, "{plan:#}");
        assert_eq!(changes[0]["reference"], "R2");
        let diagnostics = plan["diagnostics"].as_array().unwrap();
        assert_eq!(diagnostics.len(), 1, "{plan:#}");
        assert_eq!(diagnostics[0]["code"], "footprint_sheet_not_updated");
        assert_eq!(diagnostics[0]["reference"], "R1");
        assert_eq!(diagnostics[0]["footprint_id"], "Resistor_SMD:R_0402");
        let message = diagnostics[0]["message"].as_str().unwrap();
        assert!(
            message.starts_with(
                "R1 differs from the schematic only in its sheet name, sheet file or \
                 footprint filters, and owns 1 group, 1 point and variant 'NoTimer'"
            ),
            "{message}"
        );

        let applied = served.apply(TWO_RESISTORS).await;

        assert_eq!(applied["status"], "applied", "{applied:#}");
        assert_eq!(applied["coverage"]["footprints_updated"]["applied"], 1);
        assert_eq!(applied["diagnostics"], plan["diagnostics"], "{applied:#}");
        assert_eq!(served.sent("UpdateItems"), 1);
        let held = served.held.lock().unwrap().clone();
        let held_as = |kiid: &str| {
            held.iter()
                .find(|item| footprint_kiid(item) == kiid)
                .unwrap()
                .clone()
        };
        assert_eq!(held_as(OWNS_ALL_THREE), r1, "R1 is left as it was");
        let r2 = FootprintInstance::decode(held_as(OWNS_NONE).value.as_slice()).unwrap();
        assert_eq!(board_footprint_from_instance(&r2).unwrap().value, "10k");
    }

    /// With nothing else to change, the plan is a no-op that still names R1.
    #[tokio::test]
    async fn a_plan_whose_only_update_is_left_out_is_a_noop() {
        let served = served_children(vec![captured_resistor(
            OWNS_ALL_THREE,
            "R1",
            "symbol-uuid",
            "10k",
            "",
        )])
        .await;

        let (plan, applied) = dry_run_then_apply(&served).await;

        for body in [&plan, &applied] {
            assert_eq!(body["status"], "noop", "{body:#}");
            assert_eq!(body["changes"], serde_json::json!([]));
            assert_eq!(body["coverage"]["footprints_updated"]["planned"], 0);
            assert_eq!(
                body["diagnostics"][0]["code"],
                "footprint_sheet_not_updated"
            );
        }
        assert_eq!(served.sent("BeginCommit"), 0);
    }

    /// The dry run planned R1's sheet; it owns a group, a point and a variant
    /// by the apply, so the apply would leave it out, and its plan is stale.
    #[tokio::test]
    async fn a_footprint_that_gains_them_after_a_sheet_only_dry_run_makes_the_plan_stale() {
        let served = served_children(vec![captured_resistor(
            OWNS_NONE,
            "R1",
            "symbol-uuid",
            "10k",
            "",
        )])
        .await;
        let (_, plan) = served.dry_run_result(&r1_at_10k()).await;
        assert_eq!(plan["status"], "ready", "{plan:#}");

        // The same board with the two footprints' KIIDs swapped.
        served.hold_board_text(
            &FOOTPRINT_CHILDREN
                .replace(OWNS_NONE, "\u{0}")
                .replace(OWNS_ALL_THREE, OWNS_NONE)
                .replace('\u{0}', OWNS_ALL_THREE),
        );
        let (_, applied) = served
            .call(serde_json::json!({
                "schematic": served.schematic.to_string_lossy(),
                "board": served.board.to_string_lossy(),
                "dry_run": false,
                "expected_plan_revision": plan["plan_revision"],
            }))
            .await;

        assert_eq!(applied["status"], "conflict", "{applied:#}");
        let codes = applied["diagnostics"]
            .as_array()
            .unwrap()
            .iter()
            .map(|diagnostic| diagnostic["code"].as_str().unwrap())
            .collect::<Vec<_>>();
        assert!(codes.contains(&"stale_plan_revision"), "{applied:#}");
        assert_eq!(served.sent("UpdateItems"), 0);
    }

    /// A plan another update refuses lists only its conflicts: R2 is missing
    /// from the board KiCad wrote, and R1, left out, is not named.
    #[tokio::test]
    async fn a_refused_plan_lists_only_its_conflicts() {
        let missing = "00000000-0000-0000-0000-000000000836";
        let served = served_children(vec![
            captured_resistor(OWNS_ALL_THREE, "R1", "symbol-uuid", "10k", ""),
            captured_resistor(missing, "R2", "symbol-uuid-2", "1k", "/Power/"),
        ])
        .await;

        let plan = served.dry_run(TWO_RESISTORS).await;

        assert_eq!(plan["status"], "conflict", "{plan:#}");
        assert_eq!(plan["coverage"]["conflicts"]["planned"], 1, "{plan:#}");
        let diagnostics = plan["diagnostics"].as_array().unwrap();
        assert_eq!(diagnostics.len(), 1, "{plan:#}");
        assert_eq!(diagnostics[0]["code"], "footprint_children_not_carried");
        assert_eq!(diagnostics[0]["reference"], "R2");
    }

    /// A footprint that lacks its sheet and differs in anything else as well
    /// is refused as before.
    #[tokio::test]
    async fn a_footprint_that_owns_them_and_lacks_its_sheet_and_value_is_refused() {
        let served = served_children(vec![captured_resistor(
            OWNS_ALL_THREE,
            "R1",
            "symbol-uuid",
            "1k",
            "",
        )])
        .await;

        let (plan, applied) = dry_run_then_apply(&served).await;

        assert_refused(&plan, REFUSED_FOR);
        assert_refused(&applied, REFUSED_FOR);
        assert_eq!(served.sent("UpdateItems"), 0);
    }

    // ─── #69: a bounded response, verdict first ────────────────────────────

    /// `count` parts the board lacks, each a stock 0603.
    fn additions(count: usize) -> String {
        let references = (1..=count).map(|n| format!("C{n}")).collect::<Vec<_>>();
        let parts = references
            .iter()
            .map(|reference| (reference.as_str(), STOCK_0603, &["1", "2"][..]))
            .collect::<Vec<_>>();
        exported_netlist(&parts)
    }

    #[tokio::test]
    async fn a_dry_run_lists_the_first_limit_changes_and_counts_them_all() {
        let served = ServedSync::new().await;

        let (is_error, plan) = served
            .sync(&additions(3), serde_json::json!({ "limit": 1 }))
            .await;

        assert!(!is_error, "{plan:#}");
        assert_eq!(plan["status"], "ready", "{plan:#}");
        assert_eq!(plan["changes_count"], 3, "{plan:#}");
        assert_eq!(plan["changes_shown"], 1);
        assert_eq!(plan["changes_truncated"], true);
        let changes = plan["changes"].as_array().unwrap();
        assert_eq!(changes.len(), 1);
        assert_eq!(changes[0]["reference"], "C1");
        assert_eq!(plan["coverage"]["footprints_added"]["planned"], 3);
    }

    /// The input schema takes an integral `1.0` as an integer, so the limit
    /// does too.
    #[tokio::test]
    async fn an_integral_float_limit_is_the_limit() {
        let served = ServedSync::new().await;

        let (is_error, plan) = served
            .sync(&additions(3), serde_json::json!({ "limit": 1.0 }))
            .await;

        assert!(!is_error, "{plan:#}");
        assert_eq!(plan["changes_shown"], 1, "{plan:#}");
        assert_eq!(plan["changes"].as_array().unwrap().len(), 1);
    }

    #[tokio::test]
    async fn fifty_changes_are_listed_when_no_limit_is_named() {
        let served = ServedSync::new().await;

        let plan = served.dry_run(&additions(51)).await;

        assert_eq!(plan["status"], "ready", "{plan:#}");
        assert_eq!(plan["changes_count"], 51);
        assert_eq!(plan["changes_shown"], 50);
        assert_eq!(plan["changes_truncated"], true);
        assert_eq!(plan["changes"].as_array().unwrap().len(), 50);
    }

    /// `json!` sorted the keys, so `status` and `plan_revision` came after
    /// every change, past where a client cuts a long response.
    #[tokio::test]
    async fn the_verdict_and_plan_revision_come_before_the_changes() {
        let served = ServedSync::new().await;

        let (_, text) = served.sync_text(&additions(3), serde_json::json!({})).await;

        assert!(
            text.starts_with(r#"{"status":"ready","plan_revision":""#),
            "{text}"
        );
        let changes = text.find(r#""changes":["#).expect("changes");
        for key in [
            "coverage",
            "changes_count",
            "changes_shown",
            "changes_truncated",
            "staging",
            "undo",
            "diagnostics",
            "unassigned_footprints",
        ] {
            let at = text
                .find(&format!("\"{key}\":"))
                .unwrap_or_else(|| panic!("no {key}: {text}"));
            assert!(at < changes, "{key} comes after the changes: {text}");
        }
    }

    #[tokio::test]
    async fn the_output_file_holds_the_whole_response() {
        let served = ServedSync::new().await;
        let out = tempfile::tempdir().unwrap();
        // A directory that does not exist yet, as `run_drc` creates.
        let output = out.path().join("plans").join("sync.json");

        let (is_error, plan) = served
            .sync(
                &additions(3),
                serde_json::json!({ "limit": 1, "output": output.to_string_lossy() }),
            )
            .await;

        assert!(!is_error, "{plan:#}");
        assert_eq!(plan["changes_shown"], 1, "{plan:#}");
        let whole: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&output).unwrap()).unwrap();
        assert_eq!(whole["status"], plan["status"]);
        assert_eq!(whole["plan_revision"], plan["plan_revision"]);
        assert_eq!(whole["coverage"], plan["coverage"]);
        assert_eq!(
            (
                &whole["changes_count"],
                &whole["changes_shown"],
                &whole["changes_truncated"]
            ),
            (
                &serde_json::json!(3),
                &serde_json::json!(3),
                &serde_json::json!(false)
            )
        );
        let references = whole["changes"]
            .as_array()
            .unwrap()
            .iter()
            .map(|change| change["reference"].as_str().unwrap())
            .collect::<Vec<_>>();
        assert_eq!(references, ["C1", "C2", "C3"]);
    }

    /// The bound is the response's only: the revision hashes the whole plan
    /// and the apply sends all of it, and its file lists all of it.
    #[tokio::test]
    async fn a_bounded_dry_run_applies_its_whole_plan() {
        let served = ServedSync::new().await;
        let netlist = additions(3);
        let out = tempfile::tempdir().unwrap();
        let output = out.path().join("applied.json");

        let unbounded = served.dry_run(&netlist).await;
        let (_, bounded) = served
            .sync(&netlist, serde_json::json!({ "limit": 0 }))
            .await;
        assert_eq!(bounded["changes"], serde_json::json!([]), "{bounded:#}");
        assert_eq!(bounded["changes_count"], 3);
        assert_eq!(bounded["plan_revision"], unbounded["plan_revision"]);

        let (is_error, applied) = served
            .sync(
                &netlist,
                serde_json::json!({
                    "dry_run": false,
                    "expected_plan_revision": bounded["plan_revision"],
                    "limit": 0,
                    "output": output.to_string_lossy(),
                }),
            )
            .await;

        assert!(!is_error, "{applied:#}");
        assert_eq!(applied["status"], "applied", "{applied:#}");
        assert_eq!(applied["coverage"]["footprints_added"]["applied"], 3);
        assert_eq!(applied["changes"], serde_json::json!([]));
        assert_eq!(applied["changes_count"], 3);
        assert_eq!(served.sent_footprints().len(), 3);
        let whole: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&output).unwrap()).unwrap();
        assert_eq!(whole["status"], "applied", "{whole:#}");
        assert_eq!(whole["changes"].as_array().unwrap().len(), 3);
    }

    /// An output that would replace the board or a schematic of the saved
    /// hierarchy, or that cannot be a file, is refused before the board is
    /// read, as `run_drc` refuses its report path.
    #[tokio::test]
    async fn an_output_over_the_board_a_schematic_or_a_file_is_refused_first() {
        let served = ServedSync::new().await;
        // A root with one sheet, so a sheet below the root is a source too.
        std::fs::write(
            &served.schematic,
            include_bytes!("../../tests/fixtures/single_pin_nets.kicad_sch"),
        )
        .unwrap();
        let sheet = served
            .schematic
            .with_file_name("single_pin_nets_child.kicad_sch");
        std::fs::write(
            &sheet,
            include_bytes!("../../tests/fixtures/single_pin_nets_child.kicad_sch"),
        )
        .unwrap();
        let sources = [served.board.clone(), served.schematic.clone(), sheet];
        let before = sources.clone().map(|path| std::fs::read(path).unwrap());
        let out = tempfile::tempdir().unwrap();
        let blocker = out.path().join("blocker");
        std::fs::write(&blocker, "a file, not a directory").unwrap();
        let netlist = additions(1);

        for output in sources
            .iter()
            .cloned()
            .chain([out.path().to_path_buf(), blocker.join("sync.json")])
        {
            for mut extra in [
                serde_json::json!({}),
                serde_json::json!({ "dry_run": false, "expected_plan_revision": "any" }),
            ] {
                extra["output"] = serde_json::json!(output.to_string_lossy());
                let (is_error, refused) = served.sync(&netlist, extra).await;

                assert!(is_error, "{}: {refused:#}", output.display());
                assert_eq!(refused["error"]["kind"], "invalid_argument", "{refused:#}");
                assert_eq!(refused["error"]["field"], "output", "{refused:#}");
            }
        }
        assert!(served.sent_footprints().is_empty());
        assert_eq!(sources.map(|path| std::fs::read(path).unwrap()), before);
    }

    /// A write that fails after the check, as on a full disk or a permission
    /// change, is answered as `run_drc` answers a report it could not write:
    /// with nothing applied the call fails and can be repeated; after an
    /// apply it fails carrying the applied response.
    #[test]
    fn an_output_that_cannot_be_written_fails_the_call_and_keeps_an_applied_response() {
        let plan = plan_adding(vec![planned_add("C1", STOCK_0603, &["1", "2"])]);
        let out = tempfile::tempdir().unwrap();
        let blocker = out.path().join("blocker");
        std::fs::write(&blocker, "a file, not a directory").unwrap();
        let output = blocker.join("sync.json");
        let options = ResponseOptions {
            output: Some(output.clone()),
            ..ResponseOptions::default()
        };
        let body = |result: &CallToolResult| -> serde_json::Value {
            match &result.content[0] {
                ToolContent::Text { text } => serde_json::from_str(text).unwrap(),
                _ => panic!("sync response was not JSON text"),
            }
        };
        let path = output.display().to_string();

        let dry_run = sync_response(&plan, "ready", 1, false, &options);
        assert!(dry_run.is_error);
        let refused = body(&dry_run);
        assert_eq!(refused["error"]["kind"], "handler_error", "{refused:#}");
        let reason = refused["error"]["reason"].as_str().unwrap();
        assert!(reason.contains(&path), "{reason}");

        let applied = sync_response(&plan, "applied", 1, true, &options);
        assert!(applied.is_error);
        let applied = body(&applied);
        assert_eq!(applied["status"], "applied", "{applied:#}");
        assert_eq!(applied["changes_count"], 1);
        assert_eq!(applied["coverage"]["conflicts"]["planned"], 0);
        let diagnostics = applied["diagnostics"].as_array().unwrap();
        assert_eq!(diagnostics.len(), 1, "{applied:#}");
        assert_eq!(diagnostics[0]["code"], "output_not_written");
        let message = diagnostics[0]["message"].as_str().unwrap();
        assert!(message.contains(&path), "{message}");
        assert!(
            message.contains("do not apply this plan again"),
            "{message}"
        );
        assert!(!output.exists());
    }

    /// Refused by the input schema, before the handler runs: an empty
    /// `output` would otherwise reach the writer and fail there instead.
    #[tokio::test]
    async fn a_negative_limit_or_an_empty_output_is_refused_by_the_schema() {
        let served = ServedSync::new().await;

        for (field, extra) in [
            ("limit", serde_json::json!({ "limit": -1 })),
            ("output", serde_json::json!({ "output": "" })),
        ] {
            let (is_error, refused) = served.sync(&additions(1), extra).await;

            assert!(is_error, "{refused:#}");
            assert_eq!(refused["error"]["kind"], "invalid_argument", "{refused:#}");
            assert_eq!(refused["error"]["field"], field, "{refused:#}");
        }
        assert!(served.sent_footprints().is_empty());
    }

    /// Added footprints in `update_pcb_from_schematic` carry pad zone
    /// settings and reference points on KiCad 10.0.7+, and report
    /// `not_carried` on KiCad < 10.0.7 or when the footprint has clauses
    /// typed placement leaves out (#318).
    #[tokio::test]
    async fn added_footprint_carries_zone_settings_and_reference_point_and_reports_not_carried() {
        use copperpour_ipc::builders::any_is;
        use copperpour_ipc::gen::kiapi::board::types::{Pad, ReferencePoint, ZoneConnectionStyle};

        let served = ServedSync::reading_back(Readback::AsSent).await;
        let project_dir = served.board.parent().unwrap();
        let pretty = project_dir.join("Test_318.pretty");
        std::fs::create_dir_all(&pretty).unwrap();
        std::fs::write(
            pretty.join("WSON6.kicad_mod"),
            include_str!("../../tests/fixtures/issue_300_wson6_thermalvias_kicad10.kicad_mod"),
        )
        .unwrap();
        std::fs::write(
            pretty.join("DIP8.kicad_mod"),
            include_str!("../../tests/fixtures/issue_283_dip8_kicad10.kicad_mod"),
        )
        .unwrap();
        std::fs::write(
            pretty.join("WithZone.kicad_mod"),
            r#"(footprint "WithZone"
  (version 20241229)
  (generator "pcbnew")
  (layer "F.Cu")
  (property "Reference" "REF**" (at 0 -2 0) (layer "F.SilkS") (effects (font (size 1 1) (thickness 0.15))))
  (property "Value" "WithZone" (at 0 2 0) (layer "F.Fab") (effects (font (size 1 1) (thickness 0.15))))
  (pad "1" smd rect (at 0 0) (size 1 1) (layers "F.Cu" "F.Mask") (zone_connect 2))
  (zone (net 0) (net_name "") (layer "F.Cu"))
)"#,
        )
        .unwrap();
        let fp_table = project_dir.join("fp-lib-table");
        let existing_table = std::fs::read_to_string(&fp_table)
            .unwrap_or_else(|_| "(fp_lib_table\n  (version 7)\n)\n".to_string());
        let updated_table = existing_table.replacen(
            "\n)",
            "\n  (lib (name \"Test_318\")(type \"KiCad\")(uri \"${KIPRJMOD}/Test_318.pretty\")(options \"\")(descr \"\"))\n)",
            1,
        );
        std::fs::write(&fp_table, updated_table).unwrap();

        let netlist = exported_netlist(&[
            ("U1", "Test_318:WSON6", &["1", "7"]),
            ("U2", "Test_318:DIP8", &["1", "2"]),
            ("U3", "Test_318:WithZone", &["1"]),
        ]);

        let applied = served.apply(&netlist).await;
        assert_eq!(applied["status"], "applied", "{applied:#}");
        let diagnostics = applied["diagnostics"].as_array().unwrap();
        assert_eq!(diagnostics.len(), 1, "{applied:#}");
        assert_eq!(diagnostics[0]["code"], "not_carried");
        assert_eq!(diagnostics[0]["footprint_id"], "Test_318:WithZone");
        assert!(diagnostics[0]["message"].as_str().unwrap().contains("zone"));

        let sent = served.sent_footprints();
        assert_eq!(sent.len(), 3);
        let wson = sent
            .iter()
            .find(|fp| {
                fp.reference_field
                    .as_ref()
                    .unwrap()
                    .text
                    .as_ref()
                    .unwrap()
                    .text
                    .as_ref()
                    .unwrap()
                    .text
                    == "U1"
            })
            .unwrap();
        let wson_full = wson
            .definition
            .as_ref()
            .unwrap()
            .items
            .iter()
            .filter(|item| any_is(item, "kiapi.board.types.Pad"))
            .map(|item| Pad::decode(item.value.as_slice()).unwrap())
            .filter(|p| {
                p.pad_stack
                    .as_ref()
                    .and_then(|s| s.zone_settings.as_ref())
                    .is_some_and(|z| z.zone_connection == ZoneConnectionStyle::ZcsFull as i32)
            })
            .count();
        assert_eq!(wson_full, 2);

        let dip = sent
            .iter()
            .find(|fp| {
                fp.reference_field
                    .as_ref()
                    .unwrap()
                    .text
                    .as_ref()
                    .unwrap()
                    .text
                    .as_ref()
                    .unwrap()
                    .text
                    == "U2"
            })
            .unwrap();
        let rps: Vec<ReferencePoint> = dip
            .definition
            .as_ref()
            .unwrap()
            .items
            .iter()
            .filter(|item| any_is(item, "kiapi.board.types.ReferencePoint"))
            .map(|item| ReferencePoint::decode(item.value.as_slice()).unwrap())
            .collect();
        assert_eq!(rps.len(), 1);

        // On KiCad < 10.0.7, DIP8's reference point is reported in `not_carried`.
        let older = ServedSync::reading_back(Readback::AsSent).await;
        older.report_kicad_version(10, 0, 6);
        let older_dir = older.board.parent().unwrap();
        let older_pretty = older_dir.join("Test_318.pretty");
        std::fs::create_dir_all(&older_pretty).unwrap();
        std::fs::write(
            older_pretty.join("DIP8.kicad_mod"),
            include_str!("../../tests/fixtures/issue_283_dip8_kicad10.kicad_mod"),
        )
        .unwrap();
        let older_fp_table = older_dir.join("fp-lib-table");
        let older_table = std::fs::read_to_string(&older_fp_table)
            .unwrap_or_else(|_| "(fp_lib_table\n  (version 7)\n)\n".to_string())
            .replacen(
                "\n)",
                "\n  (lib (name \"Test_318\")(type \"KiCad\")(uri \"${KIPRJMOD}/Test_318.pretty\")(options \"\")(descr \"\"))\n)",
                1,
            );
        std::fs::write(&older_fp_table, older_table).unwrap();
        let dip_only = exported_netlist(&[("U2", "Test_318:DIP8", &["1", "2"])]);
        let older_applied = older.apply(&dip_only).await;
        assert_eq!(older_applied["status"], "applied", "{older_applied:#}");
        let older_diags = older_applied["diagnostics"].as_array().unwrap();
        assert_eq!(older_diags.len(), 1, "{older_applied:#}");
        assert_eq!(older_diags[0]["code"], "not_carried");
        assert!(older_diags[0]["message"]
            .as_str()
            .unwrap()
            .contains("reference point"));
    }

    // ─── #85: net-tie pad groups ────────────────────────────────────────────

    /// KiCad 10.0.8's stock three-pad net tie (`net_tie_kicad10.README.md`).
    const NET_TIE_3_THT_SOURCE: &str =
        include_str!("../../tests/fixtures/net_tie_3_tht_pad0.3mm_kicad10.kicad_mod");
    const NET_TIE_3_THT: &str = "NetTie:NetTie-3_THT_Pad0.3mm";

    fn kicad_10_0(patch: u32) -> copperpour_ipc::IpcKiCadVersion {
        copperpour_ipc::IpcKiCadVersion {
            major: 10,
            minor: 0,
            patch,
            full_version: format!("10.0.{patch}"),
        }
    }

    /// An added net tie is read with its group, and built with it when the
    /// KiCad takes it and without it when not.
    #[test]
    fn an_added_net_tie_carries_its_group_when_kicad_takes_it() {
        use copperpour_ipc::gen::kiapi;

        let part = prepare_footprint_source(NET_TIE_3_THT_SOURCE).unwrap();
        assert!(part.unsupported.is_empty(), "{:?}", part.unsupported);
        let groups = |carried: CarriedByKiCad| {
            let item = build_added_footprint(
                &planned_add("NT1", NET_TIE_3_THT, &[]),
                &part,
                &BTreeMap::new(),
                carried,
            )
            .unwrap();
            kiapi::board::types::FootprintInstance::decode(item.value.as_slice())
                .unwrap()
                .definition
                .unwrap()
                .net_ties
                .into_iter()
                .map(|group| group.pad_number)
                .collect::<Vec<_>>()
        };
        assert_eq!(groups(CarriedByKiCad::ALL), [vec!["1", "2", "3"]]);
        assert!(groups(CarriedByKiCad::default()).is_empty());

        // A group the API would not give back refuses the part, as the
        // refresh refuses it.
        let malformed = NET_TIE_3_THT_SOURCE.replace(
            "(net_tie_pad_groups \"1, 2, 3\")",
            "(net_tie_pad_groups \"1, 4\")",
        );
        assert_ne!(malformed, NET_TIE_3_THT_SOURCE, "the edit must land");
        let Err((code, reason)) = prepare_footprint_source(&malformed) else {
            panic!("a group naming a missing pad is refused");
        };
        assert_eq!(code, "unsupported_library_footprint");
        assert!(reason.contains("names pad '4'"), "{reason}");
    }

    /// Update PCB names the net ties a KiCad before 10.0.1 cannot keep, with
    /// every part that uses them, and none from 10.0.1.
    #[test]
    fn net_ties_an_older_kicad_cannot_keep_are_named() {
        let prepared = BTreeMap::from([(
            NET_TIE_3_THT.to_string(),
            prepare_footprint_source(NET_TIE_3_THT_SOURCE).unwrap(),
        )]);
        let plan = plan_adding(vec![
            planned_add("NT1", NET_TIE_3_THT, &[]),
            planned_add("NT2", NET_TIE_3_THT, &[]),
        ]);
        assert!(net_ties_not_carried(&plan, &prepared, &kicad_10_0(1)).is_empty());
        let diagnostics = net_ties_not_carried(&plan, &prepared, &kicad_10_0(0));
        assert_eq!(diagnostics.len(), 1);
        assert_eq!(diagnostics[0].code, "not_carried");
        assert_eq!(diagnostics[0].references, ["NT1", "NT2"]);
        assert!(
            diagnostics[0].message.contains(
                "net-tie pad groups (\"1, 2, 3\") require KiCad 10.0.1 or newer (connected \
                 KiCad is 10.0.0)"
            ),
            "{}",
            diagnostics[0].message
        );
    }

    /// Update PCB's replacement onto a net tie is refused on KiCad 10.0.0, as
    /// the refresh refuses it, and read from 10.0.1.
    #[test]
    fn a_replacement_onto_a_net_tie_needs_kicad_10_0_1() {
        let (temp, board_path) = project_with_stock_footprints();
        std::fs::write(
            temp.path()
                .join("Custom_Stock.pretty/NetTie-3_THT_Pad0.3mm.kicad_mod"),
            NET_TIE_3_THT_SOURCE,
        )
        .unwrap();
        let footprint_id = "Custom_Stock:NetTie-3_THT_Pad0.3mm";
        let (code, reason) =
            read_replacement(footprint_id, &board_path, &kicad_10_0(0)).unwrap_err();
        assert_eq!(code, "unsupported_library_footprint");
        assert_eq!(
            reason,
            "it has net-tie pad groups, which KiCad's API carries only from KiCad 10.0.1; this \
             KiCad is 10.0.0"
        );
        let footprint = read_replacement(footprint_id, &board_path, &kicad_10_0(1)).unwrap();
        assert!(footprint.has_net_ties());
    }

    /// Write the stock net tie to a `NetTie.pretty` library in `served`'s
    /// project, with its `fp-lib-table` row.
    fn net_tie_library(served: &ServedSync) {
        let project_dir = served.board.parent().unwrap();
        let pretty = project_dir.join("NetTie.pretty");
        std::fs::create_dir_all(&pretty).unwrap();
        std::fs::write(
            pretty.join("NetTie-3_THT_Pad0.3mm.kicad_mod"),
            NET_TIE_3_THT_SOURCE,
        )
        .unwrap();
        let fp_table = project_dir.join("fp-lib-table");
        let existing_table = std::fs::read_to_string(&fp_table)
            .unwrap_or_else(|_| "(fp_lib_table\n  (version 7)\n)\n".to_string());
        let updated_table = existing_table.replacen(
            "\n)",
            "\n  (lib (name \"NetTie\")(type \"KiCad\")(uri \"${KIPRJMOD}/NetTie.pretty\")(options \"\")(descr \"\"))\n)",
            1,
        );
        std::fs::write(&fp_table, updated_table).unwrap();
    }

    /// Update PCB adds a net tie with its library group on KiCad 10.0.8. On
    /// 10.0.0 it adds it without, asking KiCad's version once per call, and
    /// says so in a `not_carried` diagnostic.
    #[tokio::test]
    async fn an_added_net_tie_carries_its_group_or_reports_it_not_carried() {
        let netlist = exported_netlist(&[("NT1", NET_TIE_3_THT, &["1", "2", "3"])]);

        let served = ServedSync::reading_back(Readback::AsSent).await;
        net_tie_library(&served);
        let applied = served.apply(&netlist).await;
        assert_eq!(applied["status"], "applied", "{applied:#}");
        assert_eq!(applied["diagnostics"], serde_json::json!([]), "{applied:#}");
        let sent = served.sent_footprints();
        let net_ties = &sent[0].definition.as_ref().unwrap().net_ties;
        assert_eq!(net_ties.len(), 1, "{net_ties:?}");
        assert_eq!(net_ties[0].pad_number, ["1", "2", "3"]);
        assert_eq!(served.sent("GetVersion"), 2);

        let older = ServedSync::reading_back(Readback::AsSent).await;
        older.report_kicad_version(10, 0, 0);
        net_tie_library(&older);
        let applied = older.apply(&netlist).await;
        assert_eq!(applied["status"], "applied", "{applied:#}");
        let diagnostics = applied["diagnostics"].as_array().unwrap();
        assert_eq!(diagnostics.len(), 1, "{applied:#}");
        assert_eq!(diagnostics[0]["code"], "not_carried");
        assert_eq!(diagnostics[0]["footprint_id"], NET_TIE_3_THT);
        assert!(
            diagnostics[0]["message"]
                .as_str()
                .unwrap()
                .contains("require KiCad 10.0.1 or newer (connected KiCad is 10.0.0)"),
            "{applied:#}"
        );
        assert!(older.sent_footprints()[0]
            .definition
            .as_ref()
            .unwrap()
            .net_ties
            .is_empty());
    }

    /// Write each `(name, text)` footprint to a `Margins.pretty` library in
    /// `served`'s project, with its `fp-lib-table` row.
    fn margins_library(served: &ServedSync, footprints: &[(&str, &str)]) {
        let project_dir = served.board.parent().unwrap();
        let pretty = project_dir.join("Margins.pretty");
        std::fs::create_dir_all(&pretty).unwrap();
        for (name, source) in footprints {
            std::fs::write(pretty.join(format!("{name}.kicad_mod")), source).unwrap();
        }
        let fp_table = project_dir.join("fp-lib-table");
        let existing_table = std::fs::read_to_string(&fp_table)
            .unwrap_or_else(|_| "(fp_lib_table\n  (version 7)\n)\n".to_string());
        let updated_table = existing_table.replacen(
            "\n)",
            "\n  (lib (name \"Margins\")(type \"KiCad\")(uri \"${KIPRJMOD}/Margins.pretty\")(options \"\")(descr \"\"))\n)",
            1,
        );
        std::fs::write(&fp_table, updated_table).unwrap();
    }

    /// An added footprint gets the library's own solder mask and paste
    /// margins, and each pad its own on both outer layers, a margin of 0 as an
    /// override, with none of them in `not_carried`; on KiCad 10.0.6 too,
    /// which reads them as 10.0.8 does.
    #[tokio::test]
    async fn added_footprint_carries_pad_and_footprint_solder_margins() {
        use crate::tools::library_footprint::test_margins::{
            expected_margins, placed_margins, SOLDER_MARGIN_FOOTPRINTS,
        };

        let references = ["U1", "U2", "U3", "U4"];
        let ids = SOLDER_MARGIN_FOOTPRINTS.map(|(name, ..)| format!("Margins:{name}"));
        let parts: Vec<(&str, &str, &[&str])> = references
            .iter()
            .zip(&ids)
            .map(|(reference, id)| (*reference, id.as_str(), &["1", "9"][..]))
            .collect();
        for version in [(10, 0, 6), (10, 0, 8)] {
            let served = ServedSync::reading_back(Readback::AsSent).await;
            served.report_kicad_version(version.0, version.1, version.2);
            margins_library(
                &served,
                &SOLDER_MARGIN_FOOTPRINTS.map(|(name, source, ..)| (name, source)),
            );

            let applied = served.apply(&exported_netlist(&parts)).await;
            assert_eq!(applied["status"], "applied", "{applied:#}");
            assert!(
                !applied["diagnostics"].to_string().contains("solder"),
                "{applied:#}"
            );
            // On 10.0.6, pad 9's heatsink property, which needs 10.0.7, is
            // listed.
            if version == (10, 0, 8) {
                assert_eq!(applied["diagnostics"], serde_json::json!([]), "{applied:#}");
            }

            let sent = served.sent_footprints();
            assert_eq!(sent.len(), references.len());
            for (reference, (name, _, own, pad_9)) in
                references.iter().zip(SOLDER_MARGIN_FOOTPRINTS)
            {
                let placed = sent
                    .iter()
                    .find(|fp| {
                        fp.reference_field
                            .as_ref()
                            .and_then(|field| field.text.as_ref())
                            .and_then(|text| text.text.as_ref())
                            .is_some_and(|text| text.text == *reference)
                    })
                    .unwrap();
                assert_eq!(
                    placed_margins(placed),
                    expected_margins(own, pad_9),
                    "{name} on {version:?}"
                );
            }
        }
    }

    /// Inline because KiCad writes no such clause: a library footprint whose
    /// pad or own margin is not one number cannot be added; the part is
    /// skipped and named, as KiCad's Update PCB skips a part it cannot add,
    /// and the rest is still planned.
    #[tokio::test]
    async fn a_footprint_with_a_malformed_solder_margin_is_skipped_and_named() {
        use crate::tools::library_footprint::test_margins::SOLDER_MARGIN_FOOTPRINTS;

        for (owner, good, clause) in [
            (
                "pad",
                SOLDER_MARGIN_FOOTPRINTS[0].1,
                "(solder_mask_margin -0.05)",
            ),
            (
                "footprint",
                SOLDER_MARGIN_FOOTPRINTS[2].1,
                "(solder_mask_margin 0.05)",
            ),
        ] {
            assert_eq!(good.matches(clause).count(), 1, "{owner}");
            let bad = good.replace(clause, "(solder_mask_margin x)");
            let served = ServedSync::new().await;
            margins_library(&served, &[("good", good), ("bad", &bad)]);

            let plan = served
                .dry_run(&exported_netlist(&[
                    ("U1", "Margins:bad", &["1", "9"]),
                    ("U2", "Margins:good", &["1", "9"]),
                ]))
                .await;
            assert_eq!(plan["status"], "ready", "{owner}: {plan:#}");
            assert_eq!(plan["changes_count"], 1, "{owner}: {plan:#}");
            assert_eq!(plan["changes"][0]["reference"], "U2", "{owner}: {plan:#}");
            let skipped = &plan["skipped_footprints"];
            assert_eq!(skipped.as_array().unwrap().len(), 1, "{owner}: {plan:#}");
            assert_eq!(skipped[0]["code"], "unsupported_library_footprint");
            assert_eq!(skipped[0]["footprint_id"], "Margins:bad");
            assert_eq!(skipped[0]["references"], serde_json::json!(["U1"]));
            assert!(
                skipped[0]["message"].as_str().unwrap().contains(&format!(
                    "{owner} clause 'solder_mask_margin' has an invalid value 'x'"
                )),
                "{owner}: {plan:#}"
            );
        }
    }

    /// A replacement by a footprint with solder margins takes them, as KiCad's
    /// Update PCB does (`ExchangeFootprint` with its defaults): each pad gets
    /// the library's, and the footprint's own clearance, zone connection,
    /// margins and bridge setting become the library's, whatever the board
    /// had. The next dry run has nothing to do.
    #[tokio::test]
    async fn a_replacement_takes_the_new_footprints_solder_margins() {
        use crate::tools::library_footprint::test_margins::{
            expected_margins, placed_margins, SOLDER_MARGIN_FOOTPRINTS,
        };
        use copperpour_ipc::gen::kiapi::board::types::{
            FootprintInstance, SolderMaskOverrides, ZoneConnectionStyle,
        };

        for (name, source, own, pad_9) in SOLDER_MARGIN_FOOTPRINTS {
            let mut before =
                FootprintInstance::decode(schematic_backed_resistor().value.as_slice()).unwrap();
            let overrides = before.overrides.get_or_insert_with(Default::default);
            overrides.copper_clearance = Some(copperpour_ipc::builders::distance(0.2));
            overrides.solder_mask = Some(SolderMaskOverrides {
                solder_mask_margin: Some(copperpour_ipc::builders::distance(0.07)),
                ..Default::default()
            });
            overrides.zone_connection = ZoneConnectionStyle::ZcsFull as i32;
            before.attributes.as_mut().unwrap().allow_soldermask_bridges = true;
            let served = ServedSync::holding(HeldBoard {
                footprints: vec![copperpour_ipc::builders::pack_any(
                    &before,
                    "kiapi.board.types.FootprintInstance",
                )],
                zones: Vec::new(),
                nets: vec!["VCC", "GND"],
            })
            .await;
            margins_library(&served, &[(name, source)]);
            let netlist = r1_on(&format!("Margins:{name}"));

            let plan = served.dry_run(&netlist).await;
            assert_eq!(
                plan["changes"][0]["replace_footprint"]["to"],
                format!("Margins:{name}"),
                "{name}: {plan:#}"
            );
            let applied = served.apply(&netlist).await;
            assert_eq!(applied["status"], "applied", "{name}: {applied:#}");
            let held =
                FootprintInstance::decode(served.held.lock().unwrap()[0].value.as_slice()).unwrap();
            assert_eq!(
                placed_margins(&held),
                expected_margins(own, pad_9),
                "{name}"
            );
            let overrides = held.overrides.unwrap();
            assert_eq!(overrides.copper_clearance, None, "{name}");
            assert_eq!(
                overrides.zone_connection,
                ZoneConnectionStyle::ZcsInherited as i32,
                "{name}"
            );
            assert!(!held.attributes.unwrap().allow_soldermask_bridges, "{name}");
            assert_eq!(served.dry_run(&netlist).await["status"], "noop", "{name}");
        }
    }

    /// Issue #82: a KiCad-resaved hierarchy with root-page symbols, two
    /// instances of a reused sheet, reused nested sheets, and an unassigned
    /// footprint synchronizes existing footprints whose stored PCB paths carry
    /// the requested root sheet UUID prefix, while preserving their stored
    /// paths and poses and keeping native root-relative paths for additions.
    #[tokio::test]
    async fn root_prefixed_hierarchy_syncs_and_preserves_stored_paths_and_poses() {
        use copperpour_ipc::gen::kiapi;
        const ROOT_UUID: &str = "8631f762-28ca-4b46-9a77-580c49a715b8";
        const R1_REL: &str = "/cb73633d-0588-4c05-8386-c6f9a836d1b8";
        const R2_UNASSIGNED_REL: &str = "/564f8de9-7101-4413-aa04-9c4146998ec0";
        const R3_REL: &str =
            "/6fdcbd51-c009-4892-813a-cd643e2b2f7a/bc8b0001-33da-4719-a210-a76cec91947d";
        const R4_REL: &str =
            "/c311205d-f32d-47da-97d6-904ff928174e/bc8b0001-33da-4719-a210-a76cec91947d";
        const C1_REL: &str = "/6fdcbd51-c009-4892-813a-cd643e2b2f7a/b07d3f2c-88ec-482e-bf85-1c19a77fdf29/111d511c-4af3-4705-a8cc-cf287640ecc3";
        const C2_REL: &str = "/6fdcbd51-c009-4892-813a-cd643e2b2f7a/e7c7a854-5cc1-403d-8f70-a4e07226d83f/111d511c-4af3-4705-a8cc-cf287640ecc3";
        const C3_REL: &str = "/c311205d-f32d-47da-97d6-904ff928174e/e7c7a854-5cc1-403d-8f70-a4e07226d83f/111d511c-4af3-4705-a8cc-cf287640ecc3";
        const C4_REL: &str = "/c311205d-f32d-47da-97d6-904ff928174e/b07d3f2c-88ec-482e-bf85-1c19a77fdf29/111d511c-4af3-4705-a8cc-cf287640ecc3";

        let rooted = |rel: &str| format!("/{ROOT_UUID}{rel}");
        let make_fp = |kiid: &str,
                       reference: &str,
                       value: &str,
                       fp_id: &str,
                       path: &str,
                       x: f64,
                       y: f64,
                       angle: f64| {
            let source = include_str!("../../tests/fixtures/c_0603_1608metric_kicad10.kicad_mod");
            let pads = super::super::pcb_components::extract_pad_definitions(source).unwrap();
            let graphics =
                super::super::pcb_components::extract_graphic_definitions(source).unwrap();
            let fields = super::super::pcb_components::extract_field_placement(source);
            let item = copperpour_ipc::KiCadIpcClient::build_footprint_item(
                fp_id, reference, value, &pads, &graphics, &fields, x, y, angle, "F.Cu",
            )
            .unwrap();
            let mut fp = copperpour_ipc::gen::kiapi::board::types::FootprintInstance::decode(
                item.value.as_slice(),
            )
            .unwrap();
            fp.id = Some(kiapi::common::types::Kiid {
                value: kiid.to_string(),
            });
            fp.symbol_path = Some(kiapi::common::types::SheetPath {
                path: path
                    .split('/')
                    .filter(|s| !s.is_empty())
                    .map(|s| kiapi::common::types::Kiid {
                        value: s.to_string(),
                    })
                    .collect(),
                path_human_readable: String::new(),
            });
            copperpour_ipc::builders::pack_any(&fp, "kiapi.board.types.FootprintInstance")
        };

        // Mix rooted and root-relative paths on the board, plus a reference
        // rename on rooted R3 (board holds R30 -> schematic has R3) and on
        // root-relative C2 (board holds C20 -> schematic has C2), leave C4
        // absent so it is added with a native root-relative path, and keep
        // rooted R2 (unassigned) on the board.
        let held = vec![
            make_fp(
                "fp-r1",
                "R1",
                "4k7",
                "Resistor_SMD:R_0603_1608Metric",
                &rooted(R1_REL),
                12.5,
                22.5,
                90.0,
            ),
            make_fp(
                "fp-r2",
                "R2",
                "UNASSIGNED",
                "Resistor_SMD:R_0603_1608Metric",
                &rooted(R2_UNASSIGNED_REL),
                20.0,
                20.0,
                0.0,
            ),
            make_fp(
                "fp-r3",
                "R30",
                "1k",
                "Resistor_SMD:R_0603_1608Metric",
                &rooted(R3_REL),
                31.0,
                41.0,
                180.0,
            ),
            make_fp(
                "fp-r4",
                "R4",
                "1k",
                "Resistor_SMD:R_0603_1608Metric",
                R4_REL,
                42.0,
                52.0,
                0.0,
            ),
            make_fp(
                "fp-c1",
                "C1",
                "47n",
                "Capacitor_SMD:C_0603_1608Metric",
                &rooted(C1_REL),
                53.0,
                63.0,
                270.0,
            ),
            make_fp(
                "fp-c2",
                "C20",
                "100n",
                "Capacitor_SMD:C_0603_1608Metric",
                C2_REL,
                64.0,
                74.0,
                0.0,
            ),
            make_fp(
                "fp-c3",
                "C3",
                "100n",
                "Capacitor_SMD:C_0603_1608Metric",
                &rooted(C3_REL),
                75.0,
                85.0,
                0.0,
            ),
        ];

        let served = ServedSync::holding(HeldBoard {
            footprints: held,
            ..Default::default()
        })
        .await;
        served.hold_board_text(
            "(kicad_pcb\n\
               (footprint \"Resistor_SMD:R_0603_1608Metric\" (uuid \"fp-r1\"))\n\
               (footprint \"Resistor_SMD:R_0603_1608Metric\" (uuid \"fp-r2\"))\n\
               (footprint \"Resistor_SMD:R_0603_1608Metric\" (uuid \"fp-r3\"))\n\
               (footprint \"Resistor_SMD:R_0603_1608Metric\" (uuid \"fp-r4\"))\n\
               (footprint \"Capacitor_SMD:C_0603_1608Metric\" (uuid \"fp-c1\"))\n\
               (footprint \"Capacitor_SMD:C_0603_1608Metric\" (uuid \"fp-c2\"))\n\
               (footprint \"Capacitor_SMD:C_0603_1608Metric\" (uuid \"fp-c3\"))\n\
             )",
        );
        let dir = served.board.parent().unwrap();
        let root_sch = dir.join("root.kicad_sch");
        std::fs::write(
            dir.join("root.kicad_pro"),
            include_bytes!("../../tests/fixtures/pcb_sync_root_prefix_kicad10/root.kicad_pro"),
        )
        .unwrap();
        std::fs::write(
            &root_sch,
            include_bytes!("../../tests/fixtures/pcb_sync_root_prefix_kicad10/root.kicad_sch"),
        )
        .unwrap();
        std::fs::write(
            dir.join("stage.kicad_sch"),
            include_bytes!("../../tests/fixtures/pcb_sync_root_prefix_kicad10/stage.kicad_sch"),
        )
        .unwrap();
        std::fs::write(
            dir.join("leaf.kicad_sch"),
            include_bytes!("../../tests/fixtures/pcb_sync_root_prefix_kicad10/leaf.kicad_sch"),
        )
        .unwrap();
        std::fs::write(
            &served.exported,
            include_bytes!("../../tests/fixtures/pcb_sync_root_prefix_kicad10/root.net"),
        )
        .unwrap();
        let (is_error, plan) = served
            .call(serde_json::json!({
                "schematic": root_sch.to_string_lossy(),
                "board": served.board.to_string_lossy(),
                "dry_run": true,
            }))
            .await;
        assert!(!is_error, "{plan:#}");
        assert_eq!(plan["status"], "ready", "{plan:#}");
        assert_eq!(plan["coverage"]["conflicts"]["planned"], 0, "{plan:#}");
        assert_eq!(
            plan["coverage"]["footprints_added"]["planned"], 1,
            "{plan:#}"
        );
        assert_eq!(
            plan["coverage"]["board_only_preserved"]["planned"], 0,
            "{plan:#}"
        );
        assert_eq!(
            plan["coverage"]["unassigned_footprint"]["planned"], 1,
            "{plan:#}"
        );
        assert_eq!(plan["unassigned_footprints"][0]["reference"], "R2");
        assert_eq!(plan["unassigned_footprints"][0]["board_state"], "kept");

        let (apply_err, applied) = served
            .call(serde_json::json!({
                "schematic": root_sch.to_string_lossy(),
                "board": served.board.to_string_lossy(),
                "dry_run": false,
                "expected_plan_revision": plan["plan_revision"],
            }))
            .await;
        assert!(!apply_err, "{applied:#}");
        assert_eq!(applied["status"], "applied", "{applied:#}");

        // Verify rooted paths and poses were preserved on updated footprints.
        {
            let held_after = served.held.lock().unwrap();
            let find_held = |kiid: &str| {
                held_after
                    .iter()
                    .map(|any| {
                        kiapi::board::types::FootprintInstance::decode(any.value.as_slice())
                            .unwrap()
                    })
                    .find(|fp| fp.id.as_ref().map(|id| id.value.as_str()) == Some(kiid))
                    .unwrap()
            };
            let r1 = find_held("fp-r1");
            assert_eq!(
                board_symbol_path(r1.symbol_path.as_ref()).as_deref(),
                Some(rooted(R1_REL).as_str())
            );
            assert_eq!(r1.position.as_ref().unwrap().x_nm, 12_500_000);
            assert_eq!(r1.position.as_ref().unwrap().y_nm, 22_500_000);
            assert_eq!(r1.orientation.as_ref().unwrap().value_degrees, 90.0);

            let r3 = find_held("fp-r3");
            assert_eq!(field_text(&r3.reference_field), "R3");
            assert_eq!(
                board_symbol_path(r3.symbol_path.as_ref()).as_deref(),
                Some(rooted(R3_REL).as_str())
            );
            assert_eq!(r3.position.as_ref().unwrap().x_nm, 31_000_000);
            assert_eq!(r3.orientation.as_ref().unwrap().value_degrees, 180.0);

            let c1 = find_held("fp-c1");
            assert_eq!(
                board_symbol_path(c1.symbol_path.as_ref()).as_deref(),
                Some(rooted(C1_REL).as_str())
            );
            assert_eq!(c1.position.as_ref().unwrap().x_nm, 53_000_000);
            assert_eq!(c1.orientation.as_ref().unwrap().value_degrees, 270.0);

            let c2 = find_held("fp-c2");
            assert_eq!(field_text(&c2.reference_field), "C2");
            assert_eq!(
                board_symbol_path(c2.symbol_path.as_ref()).as_deref(),
                Some(C2_REL)
            );
        }

        // Verify newly added C4 carries native root-relative path.
        let added = served.sent_footprints();
        assert_eq!(added.len(), 1);
        assert_eq!(field_text(&added[0].reference_field), "C4");
        assert_eq!(
            board_symbol_path(added[0].symbol_path.as_ref()).as_deref(),
            Some(C4_REL)
        );

        // Verify a follow-up dry run against the resulting board (carrying the
        // preserved rooted paths + newly added root-relative C4) is a clean
        // noop — rooted paths do not trigger spurious updates.
        let mut after_apply = served.held.lock().unwrap().clone();
        let mut c4 = added[0].clone();
        c4.id = Some(kiapi::common::types::Kiid {
            value: "fp-c4".to_string(),
        });
        after_apply.push(copperpour_ipc::builders::pack_any(
            &c4,
            "kiapi.board.types.FootprintInstance",
        ));
        let follow_up_served = ServedSync::holding(HeldBoard {
            footprints: after_apply,
            ..Default::default()
        })
        .await;
        let follow_dir = follow_up_served.board.parent().unwrap();
        let follow_root = follow_dir.join("root.kicad_sch");
        std::fs::write(
            follow_dir.join("root.kicad_pro"),
            include_bytes!("../../tests/fixtures/pcb_sync_root_prefix_kicad10/root.kicad_pro"),
        )
        .unwrap();
        std::fs::write(
            &follow_root,
            include_bytes!("../../tests/fixtures/pcb_sync_root_prefix_kicad10/root.kicad_sch"),
        )
        .unwrap();
        std::fs::write(
            follow_dir.join("stage.kicad_sch"),
            include_bytes!("../../tests/fixtures/pcb_sync_root_prefix_kicad10/stage.kicad_sch"),
        )
        .unwrap();
        std::fs::write(
            follow_dir.join("leaf.kicad_sch"),
            include_bytes!("../../tests/fixtures/pcb_sync_root_prefix_kicad10/leaf.kicad_sch"),
        )
        .unwrap();
        std::fs::write(
            &follow_up_served.exported,
            include_bytes!("../../tests/fixtures/pcb_sync_root_prefix_kicad10/root.net"),
        )
        .unwrap();
        let (noop_err, follow_up) = follow_up_served
            .call(serde_json::json!({
                "schematic": follow_root.to_string_lossy(),
                "board": follow_up_served.board.to_string_lossy(),
                "dry_run": true,
            }))
            .await;
        assert!(!noop_err, "{follow_up:#}");
        assert_eq!(follow_up["status"], "noop", "{follow_up:#}");
    }

    /// Issue #82: foreign roots, other sheets, other symbols, and simultaneous
    /// rooted + root-relative duplicate aliases are refused before any PCB
    /// mutation over served MCP dispatch.
    #[tokio::test]
    async fn foreign_root_other_sheet_other_symbol_and_duplicate_aliases_refuse_before_mutation() {
        use copperpour_ipc::gen::kiapi;
        const ROOT_UUID: &str = "8631f762-28ca-4b46-9a77-580c49a715b8";
        const FOREIGN_ROOT: &str = "99999999-9999-4999-8999-999999999999";
        const R1_REL: &str = "/cb73633d-0588-4c05-8386-c6f9a836d1b8";
        const R4_REL: &str =
            "/c311205d-f32d-47da-97d6-904ff928174e/bc8b0001-33da-4719-a210-a76cec91947d";

        let make_fp = |kiid: &str, reference: &str, path: &str| {
            let source = include_str!("../../tests/fixtures/c_0603_1608metric_kicad10.kicad_mod");
            let pads = super::super::pcb_components::extract_pad_definitions(source).unwrap();
            let graphics =
                super::super::pcb_components::extract_graphic_definitions(source).unwrap();
            let fields = super::super::pcb_components::extract_field_placement(source);
            let item = copperpour_ipc::KiCadIpcClient::build_footprint_item(
                "Resistor_SMD:R_0603_1608Metric",
                reference,
                "10k",
                &pads,
                &graphics,
                &fields,
                10.0,
                10.0,
                0.0,
                "F.Cu",
            )
            .unwrap();
            let mut fp =
                kiapi::board::types::FootprintInstance::decode(item.value.as_slice()).unwrap();
            fp.id = Some(kiapi::common::types::Kiid {
                value: kiid.to_string(),
            });
            fp.symbol_path = Some(kiapi::common::types::SheetPath {
                path: path
                    .split('/')
                    .filter(|s| !s.is_empty())
                    .map(|s| kiapi::common::types::Kiid {
                        value: s.to_string(),
                    })
                    .collect(),
                path_human_readable: String::new(),
            });
            copperpour_ipc::builders::pack_any(&fp, "kiapi.board.types.FootprintInstance")
        };

        let cases: &[(&str, Vec<prost_types::Any>, &str)] = &[
            (
                "foreign_root",
                vec![make_fp("fp-r1", "R1", &format!("/{FOREIGN_ROOT}{R1_REL}"))],
                "reference_identity_conflict",
            ),
            (
                "other_sheet",
                vec![make_fp("fp-r3", "R3", &format!("/{ROOT_UUID}{R4_REL}"))],
                "reference_identity_conflict",
            ),
            (
                "other_symbol",
                vec![make_fp(
                    "fp-r1",
                    "R1",
                    &format!("/{ROOT_UUID}/00000000-0000-4000-8000-000000000000"),
                )],
                "reference_identity_conflict",
            ),
            (
                "duplicate_alias_rooted_and_relative",
                vec![
                    make_fp("fp-r1-rooted", "R1", &format!("/{ROOT_UUID}{R1_REL}")),
                    make_fp("fp-r1-rel", "R1_DUP", R1_REL),
                ],
                "duplicate_board_identity",
            ),
        ];

        for (label, held, expected_code) in cases {
            let served = ServedSync::holding(HeldBoard {
                footprints: held.clone(),
                ..Default::default()
            })
            .await;
            let dir = served.board.parent().unwrap();
            let root_sch = dir.join("root.kicad_sch");
            std::fs::write(
                dir.join("root.kicad_pro"),
                include_bytes!("../../tests/fixtures/pcb_sync_root_prefix_kicad10/root.kicad_pro"),
            )
            .unwrap();
            std::fs::write(
                &root_sch,
                include_bytes!("../../tests/fixtures/pcb_sync_root_prefix_kicad10/root.kicad_sch"),
            )
            .unwrap();
            std::fs::write(
                dir.join("stage.kicad_sch"),
                include_bytes!("../../tests/fixtures/pcb_sync_root_prefix_kicad10/stage.kicad_sch"),
            )
            .unwrap();
            std::fs::write(
                dir.join("leaf.kicad_sch"),
                include_bytes!("../../tests/fixtures/pcb_sync_root_prefix_kicad10/leaf.kicad_sch"),
            )
            .unwrap();
            std::fs::write(
                &served.exported,
                include_bytes!("../../tests/fixtures/pcb_sync_root_prefix_kicad10/root.net"),
            )
            .unwrap();

            let (_dry_err, dry) = served
                .call(serde_json::json!({
                    "schematic": root_sch.to_string_lossy(),
                    "board": served.board.to_string_lossy(),
                    "dry_run": true,
                }))
                .await;
            assert_eq!(dry["status"], "conflict", "{label}: {dry:#}");
            let codes: Vec<&str> = dry["diagnostics"]
                .as_array()
                .unwrap()
                .iter()
                .filter_map(|d| d["code"].as_str())
                .collect();
            assert!(
                codes.contains(expected_code),
                "{label}: expected {expected_code} in {codes:?}"
            );

            // Attempting apply with the conflict's plan revision must also
            // refuse before any IPC commit or item mutation.
            let (_apply_err, applied) = served
                .call(serde_json::json!({
                    "schematic": root_sch.to_string_lossy(),
                    "board": served.board.to_string_lossy(),
                    "dry_run": false,
                    "expected_plan_revision": dry["plan_revision"],
                }))
                .await;
            assert_eq!(applied["status"], "conflict", "{label}: {applied:#}");
            assert_eq!(
                served.sent("BeginCommit"),
                0,
                "{label}: must not begin commit"
            );
            assert_eq!(
                served.sent("CreateItems"),
                0,
                "{label}: must not create items"
            );
            assert_eq!(
                served.sent("UpdateItems"),
                0,
                "{label}: must not update items"
            );
        }
    }

    #[test]
    fn normalize_symbol_path_strips_only_exact_root_prefix() {
        let root = Some("root-uuid");
        assert_eq!(normalize_symbol_path("/root-uuid/sym", root), "/sym");
        assert_eq!(
            normalize_symbol_path("/root-uuid/sheet/sym", root),
            "/sheet/sym"
        );
        assert_eq!(
            normalize_symbol_path("/root-uuid/s1/s2/sym", root),
            "/s1/s2/sym"
        );
        assert_eq!(normalize_symbol_path("/sym", root), "/sym");
        assert_eq!(normalize_symbol_path("/sheet/sym", root), "/sheet/sym");
        assert_eq!(
            normalize_symbol_path("/other-root/sym", root),
            "/other-root/sym"
        );
        assert_eq!(
            normalize_symbol_path("/root-uuid-extra/sym", root),
            "/root-uuid-extra/sym"
        );
        assert_eq!(normalize_symbol_path("/root-uuid", root), "/root-uuid");
        assert_eq!(normalize_symbol_path("/root-uuid/", root), "/root-uuid/");
        assert_eq!(
            normalize_symbol_path("/root-uuid//sym", root),
            "/root-uuid//sym"
        );
        assert_eq!(
            normalize_symbol_path("/root-uuid/sym", None),
            "/root-uuid/sym"
        );
        assert_eq!(
            normalize_symbol_path("/root-uuid/sym", Some("")),
            "/root-uuid/sym"
        );
    }

    #[test]
    fn rooted_skipped_and_renamed_unassigned_footprints_match_by_normalized_path() {
        let design = ExportedDesign {
            root_sheet_uuid: Some("root-uuid".to_string()),
            components: Vec::new(),
            skipped: vec![SkippedComponent {
                reference: "H1".to_string(),
                symbol_path: "/h1-sym".to_string(),
            }],
            unassigned: vec![UnassignedComponent {
                reference: "R2_NEW".to_string(),
                value: "UNASSIGNED".to_string(),
                lib_id: Some("Device:R".to_string()),
                symbol_path: "/r2-sym".to_string(),
            }],
            net_names: BTreeSet::new(),
        };
        let board = board_with(vec![
            board_resistor("H1_OLD", Some("/root-uuid/h1-sym")),
            board_resistor("R2_OLD", Some("/root-uuid/r2-sym")),
        ]);
        let plan = plan_sync("netlist", &design, &board);
        assert_eq!(plan.status, PlanStatus::Noop, "{:?}", plan.diagnostics);
        // H1_OLD is claimed by the excluded H1 only through the normalized
        // path: its reference differs (#870).
        assert_eq!(plan.without_symbols.len(), 1, "{:?}", plan.without_symbols);
        assert_eq!(plan.without_symbols[0].reference, "H1_OLD");
        assert_eq!(
            plan.without_symbols[0].reason,
            WithoutSymbolReason::ExcludedFromBoard
        );
        assert_eq!(plan.unassigned.len(), 1);
        assert_eq!(plan.unassigned[0].board_state, UnassignedBoardState::Kept);
        assert_eq!(plan.counts.board_only_preserved.planned, 1);
    }
}
