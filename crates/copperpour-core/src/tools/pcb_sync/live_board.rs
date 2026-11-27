//! Read the live board and build and verify the IPC mutations for a sync.

use super::footprints::{
    build_added_footprint, listed_pad_properties, sorted_difference, update_footprint_item,
};
use super::plan::{footprint_conflict, listed};
use super::{
    AcknowledgedApply, BoardFootprint, BoardState, Bounds, CopperItem, CopperKind, CopperNetChange,
    FootprintWithoutSymbol, LiveSnapshot, PlanStatus, PlannedChange, Point, PreparedFootprint,
    RoutedCopper, StagingEvidence, SymbolSheet, SyncDiagnostic, SyncPlan,
};
use crate::tools::library::PadFabricationProperty;
use crate::tools::library_footprint::{reported_fab_property, CarriedByKiCad};
use anyhow::bail;
use anyhow::{Context, Result};
use copperpour_sexp::SexpNode;
use prost::Message;
use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

// Count bound only: this does not claim a byte-size or listener-size limit.
const SYNC_CREATE_CHUNK_SIZE: usize = 32;

pub(super) fn create_sync_items_in(
    client: &copperpour_ipc::KiCadIpcClient,
    document: &copperpour_ipc::gen::kiapi::common::types::DocumentSpecifier,
    creates: &[prost_types::Any],
) -> Result<()> {
    for chunk in creates.chunks(SYNC_CREATE_CHUNK_SIZE) {
        client.create_items_in(document.clone(), chunk.to_vec())?;
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
pub(super) struct FootprintShape {
    pub(super) pads: usize,
    pub(super) drawings: usize,
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
pub(super) fn footprint_shapes<'a>(
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
pub(super) fn verify_board_matches_what_was_sent(
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
pub(super) fn acknowledged_apply_evidence(
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
    let (report, project_rules_applied) = runtime.block_on(
        crate::tools::cli::run_drc_as_project(cli, &candidate, board, false),
    )?;
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

pub(super) fn snapshot_board(
    client: &copperpour_ipc::KiCadIpcClient,
    board: &Path,
) -> Result<LiveSnapshot> {
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
pub(super) fn board_footprint_from_instance(
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

pub(super) fn field_text(
    field: &Option<copperpour_ipc::gen::kiapi::board::types::Field>,
) -> String {
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
pub(super) fn board_symbol_path(
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

/// KiCad's version, asked at most once per call.
pub(super) fn kicad_version(
    client: &copperpour_ipc::KiCadIpcClient,
    asked: &mut Option<copperpour_ipc::IpcKiCadVersion>,
) -> Result<copperpour_ipc::IpcKiCadVersion> {
    if let Some(kicad) = asked {
        return Ok(kicad.clone());
    }
    let kicad = client.get_kicad_version()?;
    *asked = Some(kicad.clone());
    Ok(kicad)
}

/// What a footprint owns that KiCad's API cannot carry through an update (#836).
#[derive(Debug, Default, PartialEq, Eq)]
pub(super) struct NotCarried {
    pub(super) groups: usize,
    pub(super) points: usize,
    pub(super) variants: Vec<String>,
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
pub(super) fn not_carried_by_kiid(board_text: &str) -> Result<BTreeMap<String, NotCarried>> {
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
pub(super) fn updates_not_carried(
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
                             sheet file, footprint filters or symbol-unit mapping, and owns {}, \
                             which KiCad drops when its API updates a footprint; it is left out \
                             of this sync, and KiCad's Update PCB from Schematic sets them in \
                             place",
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
pub(super) struct UpdatesNotCarried {
    /// A `footprint_children_not_carried` conflict per update it refuses.
    pub(super) refused: Vec<SyncDiagnostic>,
    /// The sheet-only updates to leave out, by KIID.
    pub(super) left_out: Vec<(String, SyncDiagnostic)>,
    /// The board text could not be read, or lacks a footprint the plan
    /// updates. What an update would drop is then unknown for a reason that
    /// is not the part's, so the plan is refused whole rather than skipping
    /// each update.
    pub(super) unknown: bool,
}

/// The pads connected to copper zones, read for `plan_copper_nets` (#862).
pub(super) struct ZonePads {
    /// For each copper zone whose net is not in the schematic, the footprint
    /// pads KiCad's connectivity joins to it, as `(footprint kiid, pad
    /// number)`, sorted.
    pub(super) pads: BTreeMap<String, Vec<(String, String)>>,
    /// The KiCad holding the board cannot list them (before 10.0.1) and a
    /// zone needed them.
    pub(super) unread: bool,
    pub(super) kicad: String,
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
pub(super) fn connected_zone_pads(
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
    if !crate::tools::library_footprint::carries_net_ties(&kicad) {
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
pub(super) fn copper_net_updates(
    plan: &SyncPlan,
    snapshot: &LiveSnapshot,
) -> Result<Vec<prost_types::Any>> {
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
pub(super) fn copper_not_moved<'a>(
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

/// Each of `deletions` KiCad still lists after the commit.
pub(super) fn footprints_still_on_board(
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

pub(super) fn build_mutation_items(
    plan: &SyncPlan,
    prepared: &BTreeMap<String, PreparedFootprint>,
    replacements: &BTreeMap<String, crate::tools::pcb_footprint_update::ReplacementFootprint>,
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
                            &crate::tools::pcb_footprint_update::replace_footprint(
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
