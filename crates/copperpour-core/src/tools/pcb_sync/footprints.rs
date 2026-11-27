//! Prepare library footprints and build the footprint items the sync sends.

use super::live_board::field_text;
use super::plan::{
    changed_pad_count, conflict, footprint_conflict, listed, missing_pads, nets_for_pads,
    pad_missing_conflict,
};
use super::{
    BoardState, ExportedDesign, LiveSnapshot, PlannedChange, PreparedFootprint, SymbolField,
    SymbolSheet, SyncDiagnostic, SyncPlan,
};
use crate::tools::library::PadFabricationProperty;
use crate::tools::library_footprint::{carries_net_ties, CarriedByKiCad};
use anyhow::{bail, Context, Result};
use prost::Message;
use std::collections::{BTreeMap, BTreeSet, HashSet};
use std::path::Path;

pub(super) fn update_footprint_item(
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
pub(super) fn apply_footprint_fields(
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

pub(super) fn set_field_text(
    field: &mut Option<copperpour_ipc::gen::kiapi::board::types::Field>,
    name: &str,
    value: &str,
) {
    let field = field.get_or_insert_with(Default::default);
    field.name = name.to_string();
    let board_text = field.text.get_or_insert_with(Default::default);
    board_text.text.get_or_insert_with(Default::default).text = value.to_string();
}

/// A library footprint the plan wants to place and Copperpour could not prepare,
/// with every part that needs it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct UnpreparedFootprint {
    pub(super) footprint_id: String,
    pub(super) references: Vec<String>,
    pub(super) code: &'static str,
    pub(super) reason: String,
}

impl UnpreparedFootprint {
    pub(super) fn into_diagnostic(self) -> SyncDiagnostic {
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
    let path = crate::tools::pcb_components::resolve_footprint_file(footprint_id, board_path)
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
pub(super) fn prepare_footprint_source(
    source: &str,
) -> std::result::Result<PreparedFootprint, (&'static str, String)> {
    let unsupported =
        |error: anyhow::Error| ("unsupported_library_footprint", format!("{error:#}"));
    let pads =
        crate::tools::pcb_components::extract_pad_definitions(source).map_err(unsupported)?;
    let fields = crate::tools::pcb_components::extract_field_placement(source);
    // Read with the same readers `update_footprints_from_library` uses, so a
    // footprint the sync places carries what a refresh would give it (#789).
    let root = copperpour_sexp::parse_sexp(source)
        .context("invalid footprint S-expression")
        .map_err(unsupported)?;
    let attributes = crate::tools::library_footprint::attributes(&root).map_err(unsupported)?;
    let pad_nodes = root.find_all("pad");
    let pad_properties = crate::tools::library_footprint::pad_properties(&pad_nodes, pads.len())
        .map_err(unsupported)?;
    let pad_zones = crate::tools::library_footprint::pad_zones(&pad_nodes).map_err(unsupported)?;
    let pad_margins =
        crate::tools::library_footprint::pad_margins(&root, &pad_nodes).map_err(unsupported)?;
    let margins =
        crate::tools::library_footprint::SolderMargins::of_footprint(&root).map_err(unsupported)?;
    let points = crate::tools::library_footprint::points(&root).map_err(unsupported)?;
    let net_ties = crate::tools::library_footprint::net_ties(&root).map_err(unsupported)?;
    let unsupported_features =
        crate::tools::library_footprint::unsupported_features(&root).map_err(unsupported)?;
    let description_and_keywords = crate::tools::library_footprint::description_and_keywords(&root);
    let models = crate::tools::library_footprint::models(&root).map_err(unsupported)?;
    let library_fields =
        crate::tools::library_footprint::properties(&root).map_err(|error| format!("{error:#}"));
    // Properties travel as typed fields when they can be read. Drawing visible
    // ones as plain text as well would show them twice; that drawing is only
    // the fallback for a footprint whose properties could not be read.
    let graphics = if library_fields.is_ok() {
        crate::tools::pcb_components::extract_graphic_definitions_without_properties(source)
    } else {
        crate::tools::pcb_components::extract_graphic_definitions(source)
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
    crate::tools::library_footprint::apply_footprint_margins(footprint, &part.margins);
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
pub(in crate::tools) fn apply_library_fields(
    footprint: &mut copperpour_ipc::gen::kiapi::board::types::FootprintInstance,
    fields: &crate::tools::library_footprint::LibraryProperties,
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
        crate::tools::pcb_footprint_update::transform_library_property(
            field, &position, rotation, None,
        )
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

/// Give a footprint its symbol's sheet name, sheet file, footprint filters and
/// units, as KiCad's Update PCB does (#237, #682).
fn apply_symbol_sheet(
    footprint: &mut copperpour_ipc::gen::kiapi::board::types::FootprintInstance,
    sheet: &SymbolSheet,
) {
    use copperpour_ipc::gen::kiapi::board::types::FootprintUnitInfo;

    footprint.symbol_sheet_name = sheet.sheet_name.clone();
    footprint.symbol_sheet_filename = sheet.sheet_file.clone();
    footprint.symbol_footprint_filters = sheet.footprint_filters.clone();
    footprint.symbol_units = sheet
        .symbol_units
        .iter()
        .map(|unit| FootprintUnitInfo {
            name: unit.name.clone(),
            pin_numbers: unit.pin_numbers.clone(),
        })
        .collect();
}

/// Copy the schematic's fields onto a footprint as KiCad's Update PCB does by
/// default (#788): a field the footprint has takes the symbol's text and keeps
/// its presentation, a field it lacks is added, and a field the symbol lacks
/// stays. Datasheet and Description use their own slots, on the instance and
/// the definition as Reference and Value do; the rest are `Field` items. It
/// runs after [`apply_library_fields`], so the symbol's text wins, as in KiCad.
pub(super) fn apply_symbol_fields(
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
pub(super) fn library_fields_not_carried(
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
pub(super) fn unsupported_features_not_carried(
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
pub(super) fn kicad_10_0_7_not_carried(
    plan: &SyncPlan,
    prepared: &BTreeMap<String, PreparedFootprint>,
    kicad: &copperpour_ipc::IpcKiCadVersion,
) -> Vec<SyncDiagnostic> {
    additions_by_footprint(plan)
        .into_iter()
        .filter_map(|(footprint_id, references)| {
            let part = prepared.get(footprint_id)?;
            let reasons = crate::tools::library_footprint::not_carried_10_0_7_reasons(
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
pub(super) fn net_ties_not_carried(
    plan: &SyncPlan,
    prepared: &BTreeMap<String, PreparedFootprint>,
    kicad: &copperpour_ipc::IpcKiCadVersion,
) -> Vec<SyncDiagnostic> {
    additions_by_footprint(plan)
        .into_iter()
        .filter_map(|(footprint_id, references)| {
            let part = prepared.get(footprint_id)?;
            let reason = crate::tools::library_footprint::net_ties_not_carried_reason(
                &part.net_ties,
                kicad,
            )?;
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

/// KiCad's API serves and takes a footprint's units from 10.0.8
/// (`FootprintInstance.symbol_units`, `FOOTPRINT::Deserialize`).
pub(super) fn carries_symbol_units(kicad: &copperpour_ipc::IpcKiCadVersion) -> bool {
    (kicad.major, kicad.minor, kicad.patch) >= (10, 0, 8)
}

/// Each footprint the plan adds or updates whose symbol has units, on a KiCad
/// before 10.0.8: it is synced as before, without them, and an update can drop
/// the ones it has, as any whole-footprint IPC edit can there (#682).
pub(super) fn symbol_units_not_carried(
    plan: &SyncPlan,
    design: &ExportedDesign,
    kicad: &copperpour_ipc::IpcKiCadVersion,
) -> Vec<SyncDiagnostic> {
    let with_units = design
        .components
        .iter()
        .filter(|component| !component.sheet.symbol_units.is_empty())
        .map(|component| component.reference.as_str())
        .collect::<HashSet<_>>();
    plan.changes
        .iter()
        .filter_map(|change| {
            let (reference, done) = match change {
                PlannedChange::Add { reference, .. } => (reference, "added"),
                PlannedChange::Update { reference, .. } => (reference, "updated"),
                PlannedChange::Delete { .. } => return None,
            };
            with_units.contains(reference.as_str()).then(|| {
                conflict(
                    "symbol_units_not_carried",
                    format!(
                        "{reference} is {done} without its symbol-unit mapping, which requires \
                         KiCad 10.0.8 or newer (connected KiCad is {}); whole-footprint IPC \
                         edits before it can also drop the one a footprint has. KiCad's Update \
                         PCB from Schematic sets it. See https://github.com/Copperpour/Copperpour/blob/main/docs/TROUBLESHOOTING.md#footprint-unit-metadata-disappears-after-a-live-pcb-edit",
                        kicad.full_version
                    ),
                    Some(reference),
                )
            })
        })
        .collect()
}

/// Pad fabrication properties for a message, one entry per pad number and
/// property, bounded as [`listed`] bounds references.
pub(super) fn listed_pad_properties<'a>(
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
pub(super) fn sorted_difference<'a>(
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
pub(super) fn prepare_additions(
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
pub(super) fn prepare_replacements(
    board_path: &Path,
    plan: &SyncPlan,
    board: &BoardState,
    kicad: &copperpour_ipc::IpcKiCadVersion,
) -> (
    BTreeMap<String, crate::tools::pcb_footprint_update::ReplacementFootprint>,
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
pub(super) fn replacements_that_cannot_be_built(
    plan: &SyncPlan,
    replacements: &BTreeMap<String, crate::tools::pcb_footprint_update::ReplacementFootprint>,
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
                crate::tools::pcb_footprint_update::replace_footprint(
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
pub(super) fn read_replacement(
    footprint_id: &str,
    board_path: &Path,
    kicad: &copperpour_ipc::IpcKiCadVersion,
) -> std::result::Result<
    crate::tools::pcb_footprint_update::ReplacementFootprint,
    (&'static str, String),
> {
    let path = crate::tools::pcb_components::resolve_footprint_file(footprint_id, board_path)
        .map_err(|error| ("footprint_library_resolution_failed", format!("{error:#}")))?;
    let source = std::fs::read_to_string(&path).map_err(|error| {
        (
            "footprint_library_read_failed",
            format!("failed to read {}: {error}", path.display()),
        )
    })?;
    let footprint =
        crate::tools::pcb_footprint_update::ReplacementFootprint::parse(footprint_id, &source)
            .map_err(|error| ("unsupported_library_footprint", format!("{error:#}")))?;
    // An older KiCad would drop these on apply, as the refresh refuses it.
    if let Some(what) = footprint
        .needs_kicad_10_0_7()
        .filter(|_| !crate::tools::library_footprint::carries_kicad_10_0_7_fields(kicad))
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
pub(super) fn additions_missing_pads(
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
pub(super) fn leave_out_unconnected_pins(
    plan: &mut SyncPlan,
    prepared: &BTreeMap<String, PreparedFootprint>,
    replacements: &BTreeMap<String, crate::tools::pcb_footprint_update::ReplacementFootprint>,
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

/// One footprint the plan adds, as `CreateItems` receives it: built from the
/// library's pads, drawings and fields, given the rest of the library's data,
/// then the schematic's reference, value, symbol path, DNP and pad nets.
/// `carried` says what the KiCad it goes to takes of pad fabrication
/// properties and reference points (10.0.7+) and net-tie pad groups (10.0.1+).
pub(super) fn build_added_footprint(
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
    crate::tools::library_footprint::apply_pad_settings(
        definition,
        &part.pad_properties,
        &part.pad_zones,
        &part.pad_margins,
        carried.fields_10_0_7,
    )?;
    if carried.fields_10_0_7 {
        crate::tools::library_footprint::apply_reference_points(
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
