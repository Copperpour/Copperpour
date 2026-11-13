//! Live reads and in-place edits of placed text: a footprint's fields and
//! board-level text items.
//!
//! Both edits follow the same contract as the placed 3D-model edit: read the
//! exact item, change only the requested properties on the message KiCad sent
//! (so every property the caller did not name travels back verbatim — font
//! face, non-square glyph size, alignment, the raw `${VARIABLE}` text), send
//! it in one KiCad undo step, then prove the result with a fresh read rather
//! than echoing the request.
//!
//! Coordinates are board-absolute nanometres throughout. KiCad serializes a
//! footprint's fields in board coordinates and re-creates them verbatim on
//! update (see the `transform` module), so a field position here is never
//! relative to the footprint anchor.

use super::*;
use crate::gen::kiapi::board::types as board;
use crate::gen::kiapi::common::types as common;
use std::collections::BTreeMap;

const FOOTPRINT_INSTANCE: &str = "kiapi.board.types.FootprintInstance";
const BOARD_TEXT: &str = "kiapi.board.types.BoardText";
const FIELD: &str = "kiapi.board.types.Field";

/// A text edit or read did not identify one exact, describable item.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TextTargetError {
    /// The footprint exists but carries no such mandatory field.
    MissingField { reference: String, field: String },
    /// No board-level text item has this KIID.
    MissingBoardText { uuid: String },
    /// KiCad listed more than one board-level text item with this KIID.
    AmbiguousBoardText { uuid: String, matches: usize },
    /// KiCad's message lacked a part needed to describe or verify the text,
    /// so no edit could be proven and none was attempted.
    Unreadable { target: String, reason: String },
}

impl std::fmt::Display for TextTargetError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::MissingField { reference, field } => {
                write!(formatter, "Footprint '{reference}' has no {field} field")
            }
            Self::MissingBoardText { uuid } => write!(
                formatter,
                "No board-level text has KIID '{uuid}'; footprint fields and text boxes are not \
                 board text"
            ),
            Self::AmbiguousBoardText { uuid, matches } => write!(
                formatter,
                "KiCad listed {matches} board-level text items with KIID '{uuid}'"
            ),
            Self::Unreadable { target, reason } => {
                write!(formatter, "{target} cannot be read back: {reason}")
            }
        }
    }
}

impl std::error::Error for TextTargetError {}

fn horizontal_alignment_name(value: i32) -> String {
    use common::HorizontalAlignment as H;
    match H::try_from(value) {
        Ok(H::HaLeft) => "left",
        Ok(H::HaCenter) => "center",
        Ok(H::HaRight) => "right",
        Ok(H::HaIndeterminate) => "indeterminate",
        _ => "unknown",
    }
    .to_string()
}

fn vertical_alignment_name(value: i32) -> String {
    use common::VerticalAlignment as V;
    match V::try_from(value) {
        Ok(V::VaTop) => "top",
        Ok(V::VaCenter) => "center",
        Ok(V::VaBottom) => "bottom",
        Ok(V::VaIndeterminate) => "indeterminate",
        _ => "unknown",
    }
    .to_string()
}

fn mirrored_for_layer_side(layer: i32) -> Option<bool> {
    mirrored_for_layer_name(layer_enum_to_name(layer))
}

fn horizontal_alignment_value(alignment: IpcHorizontalAlignment) -> i32 {
    use common::HorizontalAlignment as H;
    (match alignment {
        IpcHorizontalAlignment::Left => H::HaLeft,
        IpcHorizontalAlignment::Center => H::HaCenter,
        IpcHorizontalAlignment::Right => H::HaRight,
    }) as i32
}

fn vertical_alignment_value(alignment: IpcVerticalAlignment) -> i32 {
    use common::VerticalAlignment as V;
    (match alignment {
        IpcVerticalAlignment::Top => V::VaTop,
        IpcVerticalAlignment::Center => V::VaCenter,
        IpcVerticalAlignment::Bottom => V::VaBottom,
    }) as i32
}

/// Describe a text item from KiCad's message, or say which part is missing.
///
/// Every part is required. A missing angle or stroke width is not read as
/// zero: proto3 cannot tell "zero" from "not sent" for a message-typed field,
/// and a readback that defaulted it could "prove" a value KiCad never held.
pub(crate) fn text_state(text: &board::BoardText) -> std::result::Result<IpcTextState, String> {
    let missing = |what: &str| format!("KiCad sent no {what}");
    let body = text.text.as_ref().ok_or_else(|| missing("text body"))?;
    let position = body
        .position
        .as_ref()
        .ok_or_else(|| missing("text position"))?;
    let attributes = body
        .attributes
        .as_ref()
        .ok_or_else(|| missing("text attributes"))?;
    let size = attributes
        .size
        .as_ref()
        .ok_or_else(|| missing("glyph size"))?;
    let angle = attributes
        .angle
        .as_ref()
        .ok_or_else(|| missing("text angle"))?;
    let stroke = attributes
        .stroke_width
        .as_ref()
        .ok_or_else(|| missing("stroke width"))?;
    Ok(IpcTextState {
        text: body.text.clone(),
        x_nm: position.x_nm,
        y_nm: position.y_nm,
        angle_degrees: angle.value_degrees,
        size_x_nm: size.x_nm,
        size_y_nm: size.y_nm,
        stroke_width_nm: stroke.value_nm,
        layer: layer_enum_to_name(text.layer).to_string(),
        font_name: attributes.font_name.clone(),
        horizontal_alignment: horizontal_alignment_name(attributes.horizontal_alignment),
        vertical_alignment: vertical_alignment_name(attributes.vertical_alignment),
        italic: attributes.italic,
        bold: attributes.bold,
        underlined: attributes.underlined,
        mirrored: attributes.mirrored,
        multiline: attributes.multiline,
        keep_upright: attributes.keep_upright,
        line_spacing: attributes.line_spacing,
        knockout: text.knockout,
        locked: text.locked == common::LockedState::LsLocked as i32,
    })
}

/// Whether two angles name the same direction. KiCad may normalise an angle
/// on the way in (`-90` comes back as `270`), which is not a change.
pub(crate) fn angles_match(a: f64, b: f64) -> bool {
    let difference = (a - b).rem_euclid(360.0);
    difference < 1e-6 || 360.0 - difference < 1e-6
}

/// Every property on which `observed` differs from `expected`, by name.
pub(crate) fn text_state_mismatches(
    label: &str,
    expected: &IpcTextState,
    observed: &IpcTextState,
) -> Vec<String> {
    let mut out = Vec::new();
    macro_rules! exact {
        ($($field:ident),*) => {$(
            if expected.$field != observed.$field {
                out.push(format!(
                    "{label}.{}: expected {:?}, observed {:?}",
                    stringify!($field),
                    expected.$field,
                    observed.$field
                ));
            }
        )*};
    }
    exact!(
        text,
        x_nm,
        y_nm,
        size_x_nm,
        size_y_nm,
        stroke_width_nm,
        layer,
        font_name,
        horizontal_alignment,
        vertical_alignment,
        italic,
        bold,
        underlined,
        mirrored,
        multiline,
        keep_upright,
        knockout,
        locked
    );
    if !angles_match(expected.angle_degrees, observed.angle_degrees) {
        out.push(format!(
            "{label}.angle_degrees: expected {}, observed {}",
            expected.angle_degrees, observed.angle_degrees
        ));
    }
    if (expected.line_spacing - observed.line_spacing).abs() > 1e-9 {
        out.push(format!(
            "{label}.line_spacing: expected {}, observed {}",
            expected.line_spacing, observed.line_spacing
        ));
    }
    out
}

/// Apply `edit` to a text message, touching nothing it does not name.
///
/// Attributes are never synthesised: an edit that needs them on a message
/// that lacks them is refused, because defaults would silently replace the
/// font, alignment, and size KiCad holds.
fn apply_text_edit(text: &mut board::BoardText, edit: &IpcTextEdit) -> Result<()> {
    anyhow::ensure!(
        edit.visible.is_none(),
        "visibility belongs to a footprint field, not to its text"
    );
    if let Some(layer) = &edit.layer {
        text.layer = crate::builders::try_layer_from_name(layer)? as i32;
    }
    anyhow::ensure!(
        !(edit.mirror_to_match_layer && edit.mirrored.is_some()),
        "mirroring to match the layer and an explicit mirrored flag are two answers to one \
         question"
    );
    // Resolved after the layer change, against the layer the text ends on.
    let mirrored = match edit.mirrored {
        Some(mirrored) => Some(mirrored),
        None if edit.mirror_to_match_layer => mirrored_for_layer_side(text.layer),
        None => None,
    };
    if let Some(knockout) = edit.knockout {
        text.knockout = knockout;
    }
    if let Some(locked) = edit.locked {
        // Written only when the flag really changes: KiCad may send
        // `LS_UNKNOWN` for an unlocked item, and replacing that with
        // `LS_UNLOCKED` would be an edit nobody asked for.
        let is_locked = text.locked == common::LockedState::LsLocked as i32;
        if locked != is_locked {
            text.locked = if locked {
                common::LockedState::LsLocked
            } else {
                common::LockedState::LsUnlocked
            } as i32;
        }
    }
    let body = text
        .text
        .as_mut()
        .context("KiCad sent no text body to edit")?;
    if let Some(value) = &edit.text {
        body.text = value.clone();
    }
    if let Some((x_nm, y_nm)) = edit.position_nm {
        body.position = Some(common::Vector2 { x_nm, y_nm });
    }
    let needs_attributes = edit.angle_degrees.is_some()
        || edit.size_x_nm.is_some()
        || edit.size_y_nm.is_some()
        || edit.stroke_width_nm.is_some()
        || mirrored.is_some()
        || edit.horizontal_alignment.is_some()
        || edit.vertical_alignment.is_some()
        || edit.bold.is_some()
        || edit.italic.is_some()
        || edit.keep_upright.is_some()
        || edit.font_name.is_some();
    if needs_attributes {
        let attributes = body
            .attributes
            .as_mut()
            .context("KiCad sent no text attributes; refusing to synthesise defaults")?;
        if let Some(value_degrees) = edit.angle_degrees {
            attributes.angle = Some(common::Angle { value_degrees });
        }
        if edit.size_x_nm.is_some() || edit.size_y_nm.is_some() {
            let size = attributes
                .size
                .as_mut()
                .context("KiCad sent no glyph size, so the unedited axis cannot be preserved")?;
            if let Some(x_nm) = edit.size_x_nm {
                size.x_nm = x_nm;
            }
            if let Some(y_nm) = edit.size_y_nm {
                size.y_nm = y_nm;
            }
        }
        if let Some(value_nm) = edit.stroke_width_nm {
            attributes.stroke_width = Some(common::Distance { value_nm });
        }
        if let Some(mirrored) = mirrored {
            attributes.mirrored = mirrored;
        }
        if let Some(alignment) = edit.horizontal_alignment {
            attributes.horizontal_alignment = horizontal_alignment_value(alignment);
        }
        if let Some(alignment) = edit.vertical_alignment {
            attributes.vertical_alignment = vertical_alignment_value(alignment);
        }
        if let Some(bold) = edit.bold {
            attributes.bold = bold;
        }
        if let Some(italic) = edit.italic {
            attributes.italic = italic;
        }
        if let Some(keep_upright) = edit.keep_upright {
            attributes.keep_upright = keep_upright;
        }
        if let Some(font_name) = &edit.font_name {
            attributes.font_name = font_name.clone();
        }
    }
    Ok(())
}

fn apply_field_edit(field: &mut board::Field, edit: &IpcTextEdit) -> Result<()> {
    anyhow::ensure!(
        edit.text.is_none(),
        "a footprint field's text is not edited by this operation"
    );
    if let Some(visible) = edit.visible {
        field.visible = visible;
    }
    let text = field
        .text
        .as_mut()
        .context("KiCad sent no text for this field")?;
    apply_text_edit(
        text,
        &IpcTextEdit {
            visible: None,
            ..edit.clone()
        },
    )
}

fn footprint_reference(footprint: &board::FootprintInstance) -> String {
    footprint
        .reference_field
        .as_ref()
        .and_then(|field| field.text.as_ref())
        .and_then(|text| text.text.as_ref())
        .map(|text| text.text.clone())
        .unwrap_or_default()
}

fn footprint_kiid(footprint: &board::FootprintInstance) -> String {
    footprint
        .id
        .as_ref()
        .map(|id| id.value.clone())
        .unwrap_or_default()
}

fn slot_field(
    footprint: &board::FootprintInstance,
    slot: IpcFootprintFieldSlot,
) -> Option<&board::Field> {
    match slot {
        IpcFootprintFieldSlot::Reference => footprint.reference_field.as_ref(),
        IpcFootprintFieldSlot::Value => footprint.value_field.as_ref(),
        IpcFootprintFieldSlot::Datasheet => footprint.datasheet_field.as_ref(),
        IpcFootprintFieldSlot::Description => footprint.description_field.as_ref(),
    }
}

fn slot_field_mut(
    footprint: &mut board::FootprintInstance,
    slot: IpcFootprintFieldSlot,
) -> Option<&mut board::Field> {
    match slot {
        IpcFootprintFieldSlot::Reference => footprint.reference_field.as_mut(),
        IpcFootprintFieldSlot::Value => footprint.value_field.as_mut(),
        IpcFootprintFieldSlot::Datasheet => footprint.datasheet_field.as_mut(),
        IpcFootprintFieldSlot::Description => footprint.description_field.as_mut(),
    }
}

fn field_view(
    footprint: &board::FootprintInstance,
    slot: &str,
    field: &board::Field,
) -> IpcFootprintField {
    let (state, unreadable_reason) = match field.text.as_ref() {
        None => (None, Some("KiCad sent no text for this field".to_string())),
        Some(text) => match text_state(text) {
            Ok(state) => (Some(state), None),
            Err(reason) => (None, Some(reason)),
        },
    };
    IpcFootprintField {
        footprint_reference: footprint_reference(footprint),
        footprint_kiid: footprint_kiid(footprint),
        slot: slot.to_string(),
        name: field.name.clone(),
        kiid: field
            .text
            .as_ref()
            .and_then(|text| text.id.as_ref())
            .map(|id| id.value.clone())
            .unwrap_or_default(),
        visible: field.visible,
        state,
        unreadable_reason,
        bounding_box: None,
    }
}

fn slot_view(
    footprint: &board::FootprintInstance,
    slot: IpcFootprintFieldSlot,
) -> Option<IpcFootprintField> {
    slot_field(footprint, slot).map(|field| field_view(footprint, slot.as_str(), field))
}

/// The mandatory fields KiCad sent, then every other field it carries in
/// `definition.items`.
fn footprint_fields(footprint: &board::FootprintInstance) -> Vec<IpcFootprintField> {
    let mut fields: Vec<IpcFootprintField> = IpcFootprintFieldSlot::ALL
        .into_iter()
        .filter_map(|slot| slot_view(footprint, slot))
        .collect();
    for item in footprint
        .definition
        .as_ref()
        .map(|definition| definition.items.as_slice())
        .unwrap_or_default()
    {
        // Discriminate on the declared type, never on whether a decode
        // happens to succeed (#244).
        if !crate::builders::any_is(item, FIELD) {
            continue;
        }
        match board::Field::decode(item.value.as_slice()) {
            Ok(field) => fields.push(field_view(footprint, "user", &field)),
            Err(error) => fields.push(IpcFootprintField {
                footprint_reference: footprint_reference(footprint),
                footprint_kiid: footprint_kiid(footprint),
                slot: "user".to_string(),
                name: String::new(),
                kiid: String::new(),
                visible: false,
                state: None,
                unreadable_reason: Some(format!("KiCad sent an undecodable field: {error}")),
                bounding_box: None,
            }),
        }
    }
    fields
}

/// Every difference between two views of the same field.
fn field_mismatches(expected: &IpcFootprintField, observed: &IpcFootprintField) -> Vec<String> {
    let label = expected.slot.as_str();
    let mut out = Vec::new();
    if expected.name != observed.name {
        out.push(format!(
            "{label}.name: expected {:?}, observed {:?}",
            expected.name, observed.name
        ));
    }
    if expected.kiid != observed.kiid {
        out.push(format!(
            "{label}.kiid: expected {:?}, observed {:?}",
            expected.kiid, observed.kiid
        ));
    }
    if expected.visible != observed.visible {
        out.push(format!(
            "{label}.visible: expected {}, observed {}",
            expected.visible, observed.visible
        ));
    }
    match (&expected.state, &observed.state) {
        (Some(expected), Some(observed)) => {
            out.extend(text_state_mismatches(label, expected, observed))
        }
        (None, None) => {}
        (_, None) => out.push(format!(
            "{label}: readback is not describable ({})",
            observed.unreadable_reason.as_deref().unwrap_or("unknown")
        )),
        (None, Some(_)) => out.push(format!("{label}: became describable unexpectedly")),
    }
    out
}

/// Every difference between two states of the same footprint that this
/// module's edits could plausibly touch: its pose and all mandatory fields.
fn footprint_mismatches(
    expected: &board::FootprintInstance,
    observed: &board::FootprintInstance,
) -> Vec<String> {
    let mut out = Vec::new();
    if footprint_kiid(expected) != footprint_kiid(observed) {
        out.push("footprint.kiid changed".to_string());
    }
    if expected.position != observed.position {
        out.push(format!(
            "footprint.position: expected {:?}, observed {:?}",
            expected.position, observed.position
        ));
    }
    let orientation = |footprint: &board::FootprintInstance| {
        footprint
            .orientation
            .as_ref()
            .map(|angle| angle.value_degrees)
    };
    match (orientation(expected), orientation(observed)) {
        (Some(a), Some(b)) if angles_match(a, b) => {}
        (None, None) => {}
        (a, b) => out.push(format!(
            "footprint.orientation: expected {a:?}, observed {b:?}"
        )),
    }
    if expected.layer != observed.layer {
        out.push(format!(
            "footprint.layer: expected {}, observed {}",
            layer_enum_to_name(expected.layer),
            layer_enum_to_name(observed.layer)
        ));
    }
    for slot in IpcFootprintFieldSlot::ALL {
        match (slot_view(expected, slot), slot_view(observed, slot)) {
            (Some(expected), Some(observed)) => out.extend(field_mismatches(&expected, &observed)),
            (None, None) => {}
            (Some(_), None) => out.push(format!("{} field disappeared", slot.as_str())),
            (None, Some(_)) => out.push(format!("{} field appeared", slot.as_str())),
        }
    }
    out
}

fn board_text_view(text: &board::BoardText) -> IpcBoardTextItem {
    let (state, unreadable_reason) = match text_state(text) {
        Ok(state) => (Some(state), None),
        Err(reason) => (None, Some(reason)),
    };
    IpcBoardTextItem {
        kiid: text
            .id
            .as_ref()
            .map(|id| id.value.clone())
            .unwrap_or_default(),
        state,
        unreadable_reason,
        bounding_box: None,
    }
}

fn board_text_mismatches(expected: &IpcBoardTextItem, observed: &IpcBoardTextItem) -> Vec<String> {
    let mut out = Vec::new();
    if expected.kiid != observed.kiid {
        out.push(format!(
            "kiid: expected {:?}, observed {:?}",
            expected.kiid, observed.kiid
        ));
    }
    match (&expected.state, &observed.state) {
        (Some(expected), Some(observed)) => {
            out.extend(text_state_mismatches("text", expected, observed))
        }
        (None, None) => {}
        (_, None) => out.push(format!(
            "text: readback is not describable ({})",
            observed.unreadable_reason.as_deref().unwrap_or("unknown")
        )),
        (None, Some(_)) => out.push("text: became describable unexpectedly".to_string()),
    }
    out
}

/// What a mutation followed by a fresh read established.
enum Settled {
    Applied,
    /// KiCad refused and the read proved nothing changed.
    Refused(anyhow::Error),
    Uncertain {
        mismatches: Vec<String>,
        reason: String,
    },
}

/// Decide an edit's outcome from the mutation result and readback evidence.
///
/// `to_expected` lists differences from the requested state, `to_before`
/// differences from the state before the edit; `None` means the readback
/// itself failed with the given message.
fn settle(
    mutation: Result<()>,
    readback: std::result::Result<(Vec<String>, Vec<String>), String>,
) -> Settled {
    match (mutation, readback) {
        (Ok(()), Ok((to_expected, _))) if to_expected.is_empty() => Settled::Applied,
        // KiCad reported an error but the board holds exactly what was asked.
        (Err(_), Ok((to_expected, _))) if to_expected.is_empty() => Settled::Applied,
        (Err(error), Ok((_, to_before))) if to_before.is_empty() => Settled::Refused(error),
        (Ok(()), Ok((to_expected, _))) => Settled::Uncertain {
            mismatches: to_expected,
            reason: "KiCad accepted the update, but a fresh read does not hold the requested \
                     state"
                .to_string(),
        },
        (Err(error), Ok((to_expected, _))) => Settled::Uncertain {
            mismatches: to_expected,
            reason: format!(
                "the update returned an error and a fresh read proved neither the old nor the \
                 requested state: {error:#}"
            ),
        },
        (mutation, Err(read_error)) => Settled::Uncertain {
            mismatches: Vec::new(),
            reason: format!(
                "the fresh readback failed ({read_error}); the update itself {}",
                match mutation {
                    Ok(()) => "was accepted".to_string(),
                    Err(error) => format!("returned an error: {error:#}"),
                }
            ),
        },
    }
}

impl KiCadIpcClient {
    /// Ask KiCad for the boxes of `ids`, tolerating what a read can tolerate.
    ///
    /// KiCad skips a KIID it cannot resolve rather than failing, so a missing
    /// box is reported as missing, never invented. If the request as a whole
    /// fails, the reason is returned and every box stays `None`; a listing is
    /// still useful without them.
    fn text_boxes_in(
        &self,
        document: common::DocumentSpecifier,
        ids: impl IntoIterator<Item = String>,
    ) -> (BTreeMap<String, IpcBox2>, Option<String>) {
        let requested: BTreeSet<String> = ids.into_iter().filter(|id| !id.is_empty()).collect();
        if requested.is_empty() {
            return (BTreeMap::new(), None);
        }
        let command = kiapi::common::commands::GetBoundingBox {
            header: Some(header_for(document)),
            items: requested
                .iter()
                .map(|id| common::Kiid { value: id.clone() })
                .collect(),
            mode: kiapi::common::commands::BoundingBoxMode::BbmItemOnly as i32,
        };
        let measured = (|| -> Result<BTreeMap<String, IpcBox2>> {
            let response = unpack_required::<kiapi::common::commands::GetBoundingBoxResponse>(
                self.send_command(&command, "kiapi.common.commands.GetBoundingBox")?,
                "GetBoundingBox",
            )?;
            anyhow::ensure!(
                response.items.len() == response.boxes.len(),
                "KiCad returned {} KIIDs for {} boxes",
                response.items.len(),
                response.boxes.len()
            );
            let mut boxes = BTreeMap::new();
            for (id, bounds) in response.items.into_iter().zip(response.boxes) {
                anyhow::ensure!(
                    requested.contains(&id.value),
                    "KiCad returned a box for unrequested KIID {}",
                    id.value
                );
                let position = bounds.position.context("bounding box missing position")?;
                let size = bounds.size.context("bounding box missing size")?;
                anyhow::ensure!(
                    size.x_nm >= 0 && size.y_nm >= 0,
                    "KiCad returned a negative bounding box for {}",
                    id.value
                );
                boxes.insert(
                    id.value,
                    IpcBox2 {
                        min_x_nm: position.x_nm,
                        min_y_nm: position.y_nm,
                        max_x_nm: position.x_nm + size.x_nm,
                        max_y_nm: position.y_nm + size.y_nm,
                    },
                );
            }
            Ok(boxes)
        })();
        match measured {
            Ok(boxes) => (boxes, None),
            Err(error) => (BTreeMap::new(), Some(format!("{error:#}"))),
        }
    }

    /// Every placed footprint, decoded, refusing anything that is not one.
    fn placed_footprints_in(
        &self,
        document: common::DocumentSpecifier,
    ) -> Result<Vec<board::FootprintInstance>> {
        self.get_items_in(document, common::KiCadObjectType::KotPcbFootprint)?
            .iter()
            .map(|item| {
                anyhow::ensure!(
                    crate::builders::any_is(item, FOOTPRINT_INSTANCE),
                    "KiCad answered a request for footprints with a {}",
                    item.type_url
                );
                board::FootprintInstance::decode(item.value.as_slice())
                    .context("KiCad sent an unreadable footprint")
            })
            .collect()
    }

    fn footprint_by_kiid_in(
        &self,
        document: common::DocumentSpecifier,
        kiid: &str,
    ) -> Result<board::FootprintInstance> {
        let mut matches: Vec<_> = self
            .placed_footprints_in(document)?
            .into_iter()
            .filter(|footprint| footprint_kiid(footprint) == kiid)
            .collect();
        match matches.len() {
            1 => Ok(matches.remove(0)),
            0 => anyhow::bail!("footprint {kiid} is no longer on the board"),
            n => anyhow::bail!("KiCad listed {n} footprints with KIID {kiid}"),
        }
    }

    /// The fields of one footprint (`reference`) or of every footprint, each
    /// with the box KiCad measures for it.
    pub fn list_footprint_fields_in(
        &self,
        document: common::DocumentSpecifier,
        reference: Option<&str>,
    ) -> Result<IpcFootprintFieldListing> {
        let footprints = match reference {
            Some(reference) => vec![self.find_footprint_instance_in(document.clone(), reference)?],
            None => self.placed_footprints_in(document.clone())?,
        };
        let mut fields: Vec<IpcFootprintField> =
            footprints.iter().flat_map(footprint_fields).collect();
        let (boxes, unavailable) =
            self.text_boxes_in(document, fields.iter().map(|field| field.kiid.clone()));
        for field in &mut fields {
            field.bounding_box = boxes.get(&field.kiid).copied();
        }
        Ok(IpcFootprintFieldListing {
            fields,
            bounding_boxes_unavailable: unavailable,
        })
    }

    /// Every board-level text item, decoded, refusing anything that is not one.
    fn board_texts_in(&self, document: common::DocumentSpecifier) -> Result<Vec<board::BoardText>> {
        self.get_items_in(document, common::KiCadObjectType::KotPcbText)?
            .iter()
            .map(|item| {
                anyhow::ensure!(
                    crate::builders::any_is(item, BOARD_TEXT),
                    "KiCad answered a request for board text with a {}",
                    item.type_url
                );
                board::BoardText::decode(item.value.as_slice())
                    .context("KiCad sent an unreadable board text")
            })
            .collect()
    }

    fn board_text_by_kiid_in(
        &self,
        document: common::DocumentSpecifier,
        uuid: &str,
    ) -> Result<board::BoardText> {
        let mut matches: Vec<_> = self
            .board_texts_in(document)?
            .into_iter()
            .filter(|text| text.id.as_ref().map(|id| id.value.as_str()) == Some(uuid))
            .collect();
        match matches.len() {
            1 => Ok(matches.remove(0)),
            0 => Err(TextTargetError::MissingBoardText {
                uuid: uuid.to_string(),
            }
            .into()),
            matches => Err(TextTargetError::AmbiguousBoardText {
                uuid: uuid.to_string(),
                matches,
            }
            .into()),
        }
    }

    /// Every board-level text item with the box KiCad measures for it.
    pub fn list_board_texts_in(
        &self,
        document: common::DocumentSpecifier,
    ) -> Result<IpcBoardTextListing> {
        let mut texts: Vec<IpcBoardTextItem> = self
            .board_texts_in(document.clone())?
            .iter()
            .map(board_text_view)
            .collect();
        let (boxes, unavailable) =
            self.text_boxes_in(document, texts.iter().map(|text| text.kiid.clone()));
        for text in &mut texts {
            text.bounding_box = boxes.get(&text.kiid).copied();
        }
        Ok(IpcBoardTextListing {
            texts,
            bounding_boxes_unavailable: unavailable,
        })
    }

    /// Edit one mandatory field of a placed footprint in place.
    ///
    /// The whole parent footprint is sent back — KiCad has no narrower
    /// update for a field — with only the requested properties changed, in
    /// one KiCad undo step. Success requires a fresh read in which the field
    /// holds every requested value, every other property of it and of the
    /// other mandatory fields is unchanged, and the footprint has not moved.
    /// This method never saves the board.
    pub fn edit_footprint_field_in(
        &self,
        document: common::DocumentSpecifier,
        reference: &str,
        slot: IpcFootprintFieldSlot,
        edit: &IpcTextEdit,
    ) -> Result<IpcTextEditOutcome<IpcFootprintField>> {
        anyhow::ensure!(!edit.is_empty(), "the edit names no property to change");
        let footprint = self.find_footprint_instance_in(document.clone(), reference)?;
        let kiid = footprint_kiid(&footprint);
        anyhow::ensure!(!kiid.is_empty(), "footprint '{reference}' has no KIID");
        let before = slot_view(&footprint, slot).ok_or_else(|| TextTargetError::MissingField {
            reference: reference.to_string(),
            field: slot.as_str().to_string(),
        })?;
        if let Some(reason) = &before.unreadable_reason {
            return Err(TextTargetError::Unreadable {
                target: format!("the {} field of '{reference}'", slot.as_str()),
                reason: reason.clone(),
            }
            .into());
        }

        let mut edited = footprint.clone();
        let field = slot_field_mut(&mut edited, slot)
            .context("the selected field vanished from its own copy")?;
        apply_field_edit(field, edit)?;
        let expected = slot_view(&edited, slot).context("the edited field has no view")?;
        if footprint_mismatches(&footprint, &edited).is_empty() {
            return Ok(IpcTextEditOutcome::Unchanged { current: before });
        }

        let packed = crate::builders::pack_any(&edited, FOOTPRINT_INSTANCE);
        let mutation = self.run_commit("Edit footprint field", |client| {
            client.update_items_in(document.clone(), vec![packed])
        });
        let readback = self.footprint_by_kiid_in(document, &kiid);
        let observed = readback
            .as_ref()
            .ok()
            .and_then(|after| slot_view(after, slot));
        let evidence = readback
            .as_ref()
            .map(|after| {
                (
                    footprint_mismatches(&edited, after),
                    footprint_mismatches(&footprint, after),
                )
            })
            .map_err(|error| format!("{error:#}"));
        Ok(match settle(mutation, evidence) {
            Settled::Applied => IpcTextEditOutcome::Applied {
                before,
                after: observed.context("readback proved the edit but lost the field")?,
            },
            Settled::Refused(error) => {
                return Err(error.context(
                    "KiCad rejected the field edit; a fresh read confirmed the footprint is \
                     unchanged",
                ))
            }
            Settled::Uncertain { mismatches, reason } => IpcTextEditOutcome::Uncertain {
                before,
                expected,
                observed,
                mismatches,
                reason,
            },
        })
    }

    /// Edit one board-level text item in place, by KIID.
    ///
    /// `preflight` sees the item exactly as KiCad sent it and may refuse
    /// before anything is written; the caller uses it for checks that need
    /// evidence from outside KiCad, such as the saved file. Success requires
    /// a fresh read holding every requested value and every unrequested one
    /// unchanged. One KiCad undo step; never saves the board.
    pub fn edit_board_text_in(
        &self,
        document: common::DocumentSpecifier,
        uuid: &str,
        edit: &IpcTextEdit,
        preflight: impl FnOnce(&IpcBoardTextItem) -> Result<()>,
    ) -> Result<IpcTextEditOutcome<IpcBoardTextItem>> {
        anyhow::ensure!(!edit.is_empty(), "the edit names no property to change");
        anyhow::ensure!(
            edit.visible.is_none(),
            "board text has no visibility flag; only footprint fields can be hidden"
        );
        let original = self.board_text_by_kiid_in(document.clone(), uuid)?;
        let before = board_text_view(&original);
        if let Some(reason) = &before.unreadable_reason {
            return Err(TextTargetError::Unreadable {
                target: format!("board text '{uuid}'"),
                reason: reason.clone(),
            }
            .into());
        }
        preflight(&before)?;

        let mut edited = original.clone();
        apply_text_edit(&mut edited, edit)?;
        let expected = board_text_view(&edited);
        if board_text_mismatches(&before, &expected).is_empty() {
            return Ok(IpcTextEditOutcome::Unchanged { current: before });
        }

        let packed = crate::builders::pack_any(&edited, BOARD_TEXT);
        let mutation = self.run_commit("Edit board text", |client| {
            client.update_items_in(document.clone(), vec![packed])
        });
        let observed = self
            .board_text_by_kiid_in(document, uuid)
            .map(|text| board_text_view(&text));
        let evidence = observed
            .as_ref()
            .map(|after| {
                (
                    board_text_mismatches(&expected, after),
                    board_text_mismatches(&before, after),
                )
            })
            .map_err(|error| format!("{error:#}"));
        let observed = observed.ok();
        Ok(match settle(mutation, evidence) {
            Settled::Applied => IpcTextEditOutcome::Applied {
                before,
                after: observed.context("readback proved the edit but lost the text")?,
            },
            Settled::Refused(error) => {
                return Err(error.context(
                    "KiCad rejected the board text edit; a fresh read confirmed it is unchanged",
                ))
            }
            Settled::Uncertain { mismatches, reason } => IpcTextEditOutcome::Uncertain {
                before,
                expected,
                observed,
                mismatches,
                reason,
            },
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn angles_compare_as_directions() {
        assert!(angles_match(-90.0, 270.0));
        assert!(angles_match(450.0, 90.0));
        assert!(angles_match(0.0, 360.0));
        assert!(!angles_match(90.0, 90.1));
    }

    fn sample() -> board::BoardText {
        let mut text =
            crate::builders::board_text("F.SilkS", "Rev ${REVISION}", 10.0, 20.0, 1.0, 0.0, false);
        let attributes = text.text.as_mut().unwrap().attributes.as_mut().unwrap();
        attributes.font_name = "Osifont".to_string();
        attributes.size = Some(common::Vector2 {
            x_nm: 1_200_000,
            y_nm: 1_300_000,
        });
        text
    }

    #[test]
    fn an_edit_changes_only_what_it_names() {
        let original = sample();
        let mut edited = original.clone();
        apply_text_edit(
            &mut edited,
            &IpcTextEdit {
                position_nm: Some((5_000_000, 6_000_000)),
                size_y_nm: Some(1_500_000),
                ..Default::default()
            },
        )
        .unwrap();
        let before = text_state(&original).unwrap();
        let after = text_state(&edited).unwrap();
        let mut differences = text_state_mismatches("t", &before, &after);
        differences.sort();
        assert_eq!(
            differences,
            vec![
                "t.size_y_nm: expected 1300000, observed 1500000".to_string(),
                "t.x_nm: expected 10000000, observed 5000000".to_string(),
                "t.y_nm: expected 20000000, observed 6000000".to_string(),
            ]
        );
        assert_eq!(after.text, "Rev ${REVISION}");
        assert_eq!(after.font_name, "Osifont");
        assert_eq!(after.size_x_nm, 1_200_000);
    }

    #[test]
    fn an_unrepresentable_layer_is_refused_before_anything_changes() {
        let mut text = sample();
        let error = apply_text_edit(
            &mut text,
            &IpcTextEdit {
                layer: Some("Nonsense.Layer".to_string()),
                ..Default::default()
            },
        )
        .unwrap_err();
        assert!(format!("{error:#}").contains("Nonsense.Layer"), "{error:#}");
        assert_eq!(text, sample());
    }

    #[test]
    fn attributes_are_never_synthesised() {
        let mut text = sample();
        text.text.as_mut().unwrap().attributes = None;
        let error = apply_text_edit(
            &mut text,
            &IpcTextEdit {
                angle_degrees: Some(90.0),
                ..Default::default()
            },
        )
        .unwrap_err();
        assert!(format!("{error:#}").contains("synthesise"), "{error:#}");
    }

    /// Each style attribute reaches the one proto field that carries it and
    /// shows up in the state a readback compares, so a KiCad that drops it
    /// cannot pass for success (#202).
    #[test]
    fn each_style_attribute_changes_exactly_its_own_property() {
        let original = sample();
        let before = text_state(&original).unwrap();
        let cases: [(IpcTextEdit, &str); 9] = [
            (
                IpcTextEdit {
                    mirrored: Some(true),
                    ..Default::default()
                },
                "t.mirrored: expected false, observed true",
            ),
            (
                IpcTextEdit {
                    horizontal_alignment: Some(IpcHorizontalAlignment::Right),
                    ..Default::default()
                },
                "t.horizontal_alignment: expected \"center\", observed \"right\"",
            ),
            (
                IpcTextEdit {
                    vertical_alignment: Some(IpcVerticalAlignment::Top),
                    ..Default::default()
                },
                "t.vertical_alignment: expected \"center\", observed \"top\"",
            ),
            (
                IpcTextEdit {
                    bold: Some(true),
                    ..Default::default()
                },
                "t.bold: expected false, observed true",
            ),
            (
                IpcTextEdit {
                    italic: Some(true),
                    ..Default::default()
                },
                "t.italic: expected false, observed true",
            ),
            (
                IpcTextEdit {
                    keep_upright: Some(true),
                    ..Default::default()
                },
                "t.keep_upright: expected false, observed true",
            ),
            (
                IpcTextEdit {
                    knockout: Some(true),
                    ..Default::default()
                },
                "t.knockout: expected false, observed true",
            ),
            (
                IpcTextEdit {
                    locked: Some(true),
                    ..Default::default()
                },
                "t.locked: expected false, observed true",
            ),
            (
                IpcTextEdit {
                    font_name: Some(String::new()),
                    ..Default::default()
                },
                "t.font_name: expected \"Osifont\", observed \"\"",
            ),
        ];
        for (edit, expected) in cases {
            let mut edited = original.clone();
            apply_text_edit(&mut edited, &edit).unwrap();
            assert_eq!(
                text_state_mismatches("t", &before, &text_state(&edited).unwrap()),
                vec![expected.to_string()],
                "{edit:?}"
            );
        }
    }

    /// KiCad may report an unlocked item as `LS_UNKNOWN`. Asking for the
    /// state it already has must not rewrite the message, or the edit would
    /// send an update for a change nobody can observe.
    #[test]
    fn restating_the_lock_state_leaves_the_message_alone() {
        let mut text = sample();
        text.locked = common::LockedState::LsUnknown as i32;
        let original = text.clone();
        apply_text_edit(
            &mut text,
            &IpcTextEdit {
                locked: Some(false),
                ..Default::default()
            },
        )
        .unwrap();
        assert_eq!(text, original);
    }

    fn mirrored_after(start: &board::BoardText, layer: Option<&str>) -> bool {
        let mut text = start.clone();
        apply_text_edit(
            &mut text,
            &IpcTextEdit {
                layer: layer.map(str::to_string),
                mirror_to_match_layer: true,
                ..Default::default()
            },
        )
        .unwrap();
        text_state(&text).unwrap().mirrored
    }

    /// Mirroring to match the layer follows the side the text ends on, not
    /// the flag it started with (#202).
    #[test]
    fn mirroring_to_match_the_layer_follows_the_side_the_text_ends_on() {
        let front = sample();
        let mut back_unmirrored = sample();
        back_unmirrored.layer = board::BoardLayer::BlBSilkS as i32;
        let mut back_mirrored = back_unmirrored.clone();
        back_mirrored
            .text
            .as_mut()
            .unwrap()
            .attributes
            .as_mut()
            .unwrap()
            .mirrored = true;

        assert!(mirrored_after(&front, Some("B.SilkS")));
        assert!(mirrored_after(&front, Some("B.Fab")));
        assert!(!mirrored_after(&back_mirrored, Some("F.SilkS")));
        assert!(!mirrored_after(&front, Some("F.Fab")));
        // With no layer named, the layer KiCad holds decides: this is the
        // text that was moved to the back before anything could mirror it.
        assert!(mirrored_after(&back_unmirrored, None));
        assert!(mirrored_after(&back_mirrored, None));
        assert!(!mirrored_after(&front, None));
        // A layer with no side leaves the flag as it was, either way.
        for sideless in ["Edge.Cuts", "In1.Cu", "Dwgs.User"] {
            assert!(!mirrored_after(&front, Some(sideless)), "{sideless}");
            assert!(mirrored_after(&back_mirrored, Some(sideless)), "{sideless}");
        }
    }

    #[test]
    fn mirroring_to_match_the_layer_refuses_an_explicit_flag_as_well() {
        let mut text = sample();
        let error = apply_text_edit(
            &mut text,
            &IpcTextEdit {
                mirrored: Some(false),
                mirror_to_match_layer: true,
                ..Default::default()
            },
        )
        .unwrap_err();
        assert!(format!("{error:#}").contains("two answers"), "{error:#}");
    }

    #[test]
    fn a_style_attribute_is_never_set_on_synthesised_attributes() {
        let mut text = sample();
        text.text.as_mut().unwrap().attributes = None;
        let error = apply_text_edit(
            &mut text,
            &IpcTextEdit {
                mirrored: Some(true),
                ..Default::default()
            },
        )
        .unwrap_err();
        assert!(format!("{error:#}").contains("synthesise"), "{error:#}");
    }

    #[test]
    fn a_missing_angle_is_unreadable_not_zero() {
        let mut text = sample();
        text.text
            .as_mut()
            .unwrap()
            .attributes
            .as_mut()
            .unwrap()
            .angle = None;
        assert_eq!(
            text_state(&text).unwrap_err(),
            "KiCad sent no text angle".to_string()
        );
    }

    #[test]
    fn settle_distinguishes_applied_refused_and_uncertain() {
        assert!(matches!(
            settle(Ok(()), Ok((vec![], vec!["x".into()]))),
            Settled::Applied
        ));
        assert!(matches!(
            settle(Err(anyhow::anyhow!("no")), Ok((vec!["x".into()], vec![]))),
            Settled::Refused(_)
        ));
        assert!(matches!(
            settle(Ok(()), Ok((vec!["x".into()], vec![]))),
            Settled::Uncertain { .. }
        ));
        assert!(matches!(
            settle(Ok(()), Err("gone".into())),
            Settled::Uncertain { .. }
        ));
    }
}
