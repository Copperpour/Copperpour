//! Component property, field placement, DNP attribute, and designator editing.

use super::deletion::ComponentDeleteTargetError;
use super::readback::{
    commit_verified_component_mutation, component_target_from_source, copy_component_observation,
    load_component_mutation_readback, ComponentMutationIntent,
};
use crate::mcp::error::ToolErrorKind;
use crate::mcp::protocol::CallToolResult;
use crate::tools::footprint_metadata::{indent_before, quote_string};
use crate::tools::symbol_fields;
use crate::tools::{
    find_all_symbol_instance_blocks, get_path, invalid_arg, opt_str, opt_u32, require_str,
    ToolContext,
};
use copperpour_schematic_editor as cse;
use copperpour_sexp::{
    parse_sexp,
    schematic::parse_at,
    writer::{
        apply_edits, find_block_with_leading_whitespace, find_direct_child_blocks, read_consistent,
        SexpEdit,
    },
    SchematicCommand,
};
use serde_json::json;
use std::collections::BTreeMap;

/// Properties this tool exposes as first-class parameters. Routing one of them
/// through `fields` too would let a single call set the same property twice
/// with different values, and for Reference it would skip the instances-path
/// rewrite entirely — a rename that the netlist ignores (#157).
pub(super) fn is_reserved_property(name: &str) -> bool {
    matches!(name, "Reference" | "Value" | "Footprint" | "Datasheet") || is_dnp_property(name)
}

/// KiCad's do-not-populate state is the symbol attribute `(dnp yes|no)`, not
/// a property. A property named DNP misleads: KiCad's plot draws the part as
/// fitted and `kicad-cli sch export bom --exclude-dnp` keeps it, while the
/// property fills the BOM's DNP column, so writing one reports success and
/// the BOM looks right while the part stays fitted (#415). Any case is
/// refused, because every spelling makes the same mistake.
pub(crate) fn is_dnp_property(name: &str) -> bool {
    name.eq_ignore_ascii_case("dnp")
}

/// The refusal for a property named DNP, pointing at the native argument.
pub(crate) fn dnp_property_refusal(name: &str) -> String {
    format!(
        "'{name}' is not a property: KiCad's do-not-populate state is the symbol's \
         native attribute — set it with the 'dnp' argument"
    )
}

/// The tag of an S-expression block's text, `(tag …)`.
fn block_tag(block: &str) -> &str {
    let inner = &block[1..];
    let end = inner
        .find(|c: char| c.is_ascii_whitespace() || c == '(' || c == ')')
        .unwrap_or(inner.len());
    &inner[..end]
}

/// The symbol header tokens eeschema writes, in its order, up to `(dnp …)`.
/// A flag a file does not carry, such as `(dnp …)` in one from before KiCad 7,
/// goes after the last token before it here that the symbol has.
const SYMBOL_HEADER_TOKENS: [&str; 11] = [
    "lib_id",
    "at",
    "mirror",
    "unit",
    "convert",
    "body_style",
    "exclude_from_sim",
    "in_bom",
    "on_board",
    "in_pos_files",
    "dnp",
];

/// Set a native `(flag yes|no)` attribute — `dnp`, `in_bom` or `on_board` —
/// in each placed-symbol block of `blocks`, rewriting the existing token in
/// place. Only a symbol's own direct children are considered: a KiCad 10
/// variant's `(dnp …)` inside `(instances …)`, or text inside a property
/// value, is never touched.
fn set_flag_in_blocks(
    content: &str,
    blocks: &[(usize, usize)],
    reference: &str,
    flag: &str,
    value: bool,
) -> Result<String, String> {
    let token = format!("({flag} {})", if value { "yes" } else { "no" });
    let before = SYMBOL_HEADER_TOKENS
        .iter()
        .position(|tag| *tag == flag)
        .map_or(&SYMBOL_HEADER_TOKENS[..], |at| &SYMBOL_HEADER_TOKENS[..at]);
    let mut edits = Vec::new();
    for &(start, end) in blocks {
        let block = &content[start..end];
        let children = find_direct_child_blocks(block, "symbol");
        let tagged = |tag: &str| {
            children
                .iter()
                .copied()
                .filter(|&(child_start, child_end)| {
                    block_tag(&block[child_start..child_end]) == tag
                })
                .collect::<Vec<_>>()
        };
        match tagged(flag).as_slice() {
            [(child_start, child_end)] => edits.push(SexpEdit::replace(
                start + child_start,
                start + child_end,
                token.clone(),
            )),
            [] => {
                let anchor = children
                    .iter()
                    .copied()
                    .filter(|&(child_start, child_end)| {
                        before.contains(&block_tag(&block[child_start..child_end]))
                    })
                    .max_by_key(|&(_, child_end)| child_end)
                    .ok_or_else(|| {
                        format!("a placed unit of '{reference}' has no symbol header to follow")
                    })?;
                // On its own line at the anchor's indentation and with the
                // file's line ending, as eeschema writes it; on a header line
                // shared with other tokens (KiCad 6), inline.
                let line_start = block[..anchor.0].rfind('\n').map_or(0, |at| at + 1);
                let indent = &block[line_start..anchor.0];
                let newline = if block[..line_start].ends_with("\r\n") {
                    "\r\n"
                } else {
                    "\n"
                };
                let text = if indent.trim().is_empty() {
                    format!("{newline}{indent}{token}")
                } else {
                    format!(" {token}")
                };
                edits.push(SexpEdit::insert(start + anchor.1, text));
            }
            _ => {
                return Err(format!(
                    "a placed unit of '{reference}' carries more than one ({flag} …) attribute"
                ))
            }
        }
    }
    Ok(apply_edits(content.to_string(), edits))
}

/// Set the native `(dnp yes|no)` attribute on every placed unit whose
/// Reference is `reference`. Returns the edited source and the number of
/// placed units written.
pub(crate) fn set_dnp_attribute(
    content: &str,
    reference: &str,
    dnp: bool,
) -> Result<(String, usize), String> {
    let blocks = find_all_symbol_instance_blocks(content, reference);
    if blocks.is_empty() {
        return Err(format!("symbol '{reference}' not found in this schematic"));
    }
    Ok((
        set_flag_in_blocks(content, &blocks, reference, "dnp", dnp)?,
        blocks.len(),
    ))
}

/// The top-level `(symbol …)` blocks whose own `(uuid …)` is one of `uuids`:
/// a bound component's units, found by identity rather than by Reference
/// text that another component can share (after a rename onto it, say).
fn symbol_blocks_by_uuid(content: &str, uuids: &[String]) -> Vec<(usize, usize)> {
    find_direct_child_blocks(content, "kicad_sch")
        .into_iter()
        .filter(|&(start, end)| {
            let block = &content[start..end];
            block_tag(block) == "symbol"
                && find_direct_child_blocks(block, "symbol")
                    .into_iter()
                    .filter(|&(child_start, child_end)| {
                        block_tag(&block[child_start..child_end]) == "uuid"
                    })
                    .any(|(child_start, child_end)| {
                        parse_sexp(&block[child_start..child_end])
                            .ok()
                            .and_then(|node| {
                                node.get(1)
                                    .and_then(|uuid| uuid.as_str())
                                    .map(str::to_owned)
                            })
                            .is_some_and(|uuid| uuids.contains(&uuid))
                    })
        })
        .collect()
}

/// KiCad's mandatory symbol fields. Every placed symbol carries them, and
/// eeschema reads a property as one of them whatever its case, so a lowercase
/// `value` is still the Value field.
fn is_mandatory_field(name: &str) -> bool {
    [
        "Reference",
        "Value",
        "Footprint",
        "Datasheet",
        "Description",
    ]
    .iter()
    .any(|mandatory| mandatory.eq_ignore_ascii_case(name))
}

/// The `remove_fields` names, checked in full before anything is edited so a
/// refusal leaves the file untouched. A repeated name is removed once.
///
/// Omission alone is read as no argument. An explicit `null` is refused like
/// any other value that is not an array, as the advertised schema refuses it,
/// so a direct call and a served one get the same answer.
fn remove_field_names(args: &serde_json::Value) -> Result<Vec<String>, CallToolResult> {
    let entries = match args.get("remove_fields") {
        None => return Ok(Vec::new()),
        Some(serde_json::Value::Array(entries)) => entries,
        Some(_) => {
            return Err(crate::tools::invalid_arg(
                "remove_fields",
                "expected an array of field names",
            ))
        }
    };
    let mut names: Vec<String> = Vec::new();
    for entry in entries {
        let Some(name) = entry.as_str().filter(|name| !name.is_empty()) else {
            return Err(crate::tools::invalid_arg(
                "remove_fields",
                "every entry must be a non-empty field name",
            ));
        };
        if is_mandatory_field(name) {
            return Err(crate::tools::invalid_arg(
                "remove_fields",
                &format!(
                    "'{name}' is one of KiCad's mandatory symbol fields (Reference, Value, \
                     Footprint, Datasheet, Description), which cannot be removed"
                ),
            ));
        }
        for argument in ["fields", "field_placements"] {
            if args[argument].get(name).is_some() {
                return Err(crate::tools::invalid_arg(
                    "remove_fields",
                    &format!("'{name}' is also named in {argument}; a call cannot both change and remove a field"),
                ));
            }
        }
        if !names.iter().any(|seen| seen == name) {
            names.push(name.to_owned());
        }
    }
    Ok(names)
}

#[derive(Debug, Clone, Copy)]
pub(crate) struct PropertyWriteCounts {
    pub(crate) updated: usize,
    pub(crate) added: usize,
}

pub(super) fn escape_property_text(value: &str) -> String {
    value
        .replace('\\', "\\\\")
        .replace('"', "\\\"")
        .replace('\n', "\\n")
        .replace('\r', "\\r")
        .replace('\t', "\\t")
}

pub(super) fn closing_quote(content: &str, value_start: usize) -> Option<usize> {
    let mut escaped = false;
    for (offset, ch) in content[value_start..].char_indices() {
        if escaped {
            escaped = false;
        } else if ch == '\\' {
            escaped = true;
        } else if ch == '"' {
            return Some(value_start + offset);
        }
    }
    None
}

/// Build the insertion for a custom property in one placed unit's block.
///
/// The property is anchored at that unit's own placement and inherits its
/// indentation, so applying this to every block neither piles fields at the
/// origin nor rewrites an eeschema-formatted file wholesale.
fn property_insert_edit(
    content: &str,
    reference: &str,
    (start, end): (usize, usize),
    name: &str,
    value: &str,
    private: bool,
) -> Result<SexpEdit, String> {
    let block = &content[start..end];

    let (x, y, _) = parse_sexp(block)
        .ok()
        .and_then(|node| parse_at(&node))
        .ok_or_else(|| format!("'{reference}' has no readable (at …) placement"))?;

    // Match the block's own indentation rather than assuming: eeschema saves
    // with tabs, this crate's writer uses two spaces.
    let indent = find_direct_child_blocks(block, "symbol")
        .into_iter()
        .find(|&(from, _)| {
            symbol_fields::header(block, from).is_some_and(|(tag, _)| tag == "property")
        })
        .and_then(|(from, _)| indent_before(content, start + from))
        .unwrap_or_else(|| "\t\t".to_string());

    let flag = if private { "private " } else { "" };
    let newline = if content.contains("\r\n") {
        "\r\n"
    } else {
        "\n"
    };
    let prop = format!(
        "{newline}{indent}(property {flag}{} {}{newline}\
         {indent}\t(at {x} {y} 0){newline}\
         {indent}\t(hide yes){newline}\
         {indent}\t(effects{newline}\
         {indent}\t\t(font{newline}\
         {indent}\t\t\t(size 1.27 1.27){newline}\
         {indent}\t\t){newline}\
         {indent}\t){newline}\
         {indent})",
        quote_string(name),
        quote_string(value)
    );

    // Insert before the block's closing paren so the property stays inside it.
    let close = block
        .rfind(')')
        .map(|offset| start + offset)
        .ok_or_else(|| format!("symbol block for '{reference}' is malformed"))?;
    Ok(SexpEdit::insert(close, prop))
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum PropertyWriteError {
    Malformed(String),
    AmbiguousVisibility(String),
}

impl std::fmt::Display for PropertyWriteError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Malformed(message) | Self::AmbiguousVisibility(message) => f.write_str(message),
        }
    }
}

/// Set one shared component property in every placed unit.
///
/// Built-in properties must already exist in every unit (`add_missing=false`).
/// Custom fields may be present on only some units in a legacy/broken sheet;
/// `add_missing=true` updates those copies and fills the missing ones in the
/// same atomic document command.
pub(crate) fn set_property_value(
    content: &str,
    reference: &str,
    field: &str,
    new_value: &str,
    add_missing: bool,
) -> Result<(String, PropertyWriteCounts), PropertyWriteError> {
    let blocks: Vec<_> = find_direct_child_blocks(content, "kicad_sch")
        .into_iter()
        .filter(|&(start, end)| {
            let block = &content[start..end];
            block_tag(block) == "symbol"
                && parse_sexp(block).is_ok_and(|node| {
                    node.find_str("lib_id").is_some()
                        && node.find_all("property").into_iter().any(|property| {
                            property.get(1).and_then(|name| name.as_str()) == Some("Reference")
                                && property.get(2).and_then(|value| value.as_str())
                                    == Some(reference)
                        })
                })
        })
        .collect();
    set_property_value_in_blocks(content, &blocks, reference, field, new_value, add_missing)
}

fn set_property_value_in_blocks(
    content: &str,
    blocks: &[(usize, usize)],
    reference: &str,
    field: &str,
    new_value: &str,
    add_missing: bool,
) -> Result<(String, PropertyWriteCounts), PropertyWriteError> {
    if blocks.is_empty() {
        return Err(PropertyWriteError::Malformed(format!(
            "symbol '{reference}' not found in this schematic"
        )));
    }

    let value = quote_string(new_value);
    let mut edits = Vec::new();
    let mut missing = Vec::new();
    let (mut has_private, mut has_public) = (false, false);

    for &(start, end) in blocks {
        let properties = symbol_fields::symbol_properties(content, start, end)
            .map_err(|error| PropertyWriteError::Malformed(format!("'{reference}': {error}")))?;
        if let Some(property) = properties
            .into_iter()
            .find(|property| property.name == field)
        {
            has_private |= property.private;
            has_public |= !property.private;
            edits.push(SexpEdit::replace(
                property.value_span.0,
                property.value_span.1,
                value.clone(),
            ));
        } else {
            missing.push((start, end));
        }
    }
    if !missing.is_empty() {
        if !add_missing {
            if edits.is_empty() {
                return Err(PropertyWriteError::Malformed(format!(
                    "Field '{field}' not found on '{reference}'"
                )));
            }
            return Err(PropertyWriteError::Malformed(format!(
                "'{reference}' is missing the shared '{field}' property on one of its placed units"
            )));
        }
        if has_private && has_public {
            return Err(PropertyWriteError::AmbiguousVisibility(format!(
                "'{reference}' has both private and public copies of '{field}'; \
                 cannot choose which to add to the missing units"
            )));
        }
    }

    let counts = PropertyWriteCounts {
        updated: edits.len(),
        added: missing.len(),
    };
    for block in missing {
        edits.push(
            property_insert_edit(content, reference, block, field, new_value, has_private)
                .map_err(PropertyWriteError::Malformed)?,
        );
    }

    Ok((apply_edits(content.to_string(), edits), counts))
}

/// Plan the deletion of one property from every placed unit: the edits, and
/// how many units carried it. A unit without it is left alone, so removing a
/// field that is already gone plans nothing.
///
/// Private fields cannot be removed through `remove_fields`.
fn plan_property_removal(
    path: &std::path::Path,
    content: &str,
    blocks: &[(usize, usize)],
    reference: &str,
    field: &str,
) -> Result<(Vec<SexpEdit>, usize), CallToolResult> {
    let malformed = |why: String| {
        let message = format!("No fields were updated on '{reference}': {field}: {why}");
        super::malformed_schematic_error(path, message)
    };
    if blocks.is_empty() {
        let message = format!(
            "No fields were updated on '{reference}': {field}: symbol '{reference}' not found in this schematic"
        );
        return Err(CallToolResult::error_kind(
            ToolErrorKind::StaleTarget {
                target: format!("component {reference}"),
                reason: message.clone(),
            },
            message,
        ));
    }

    let mut edits = Vec::new();

    for &(start, end) in blocks {
        let properties =
            symbol_fields::symbol_properties(content, start, end).map_err(&malformed)?;
        if let Some(property) = properties
            .into_iter()
            .find(|property| property.name == field)
        {
            if property.private {
                return Err(invalid_arg(
                    "remove_fields",
                    &format!(
                        "'{field}' is a private field on '{reference}', which this tool does \
                         not remove; nothing was changed"
                    ),
                ));
            }
            let (from, to) = find_block_with_leading_whitespace(content, property.block.0)
                .ok_or_else(|| {
                    malformed(format!("'{field}' property on '{reference}' is malformed"))
                })?;
            edits.push(SexpEdit::delete(from, to));
        }
    }

    let units = edits.len();
    Ok((edits, units))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum EditErrorKind {
    InvalidArgument { field: &'static str },
    PlanBlocked,
    StaleTarget,
    MalformedFile,
}

#[derive(Default)]
struct FieldPlacementEdits {
    changes: Vec<String>,
    errors: Vec<String>,
    error_kinds: Vec<EditErrorKind>,
    applied: Vec<(String, FieldPlacement)>,
    unit: Option<u32>,
}

pub(crate) struct FieldPlacementFailure {
    pub(crate) error: CallToolResult,
    pub(crate) abort_batch: bool,
}

impl FieldPlacementFailure {
    fn local(error: CallToolResult) -> Self {
        Self {
            error,
            abort_batch: false,
        }
    }

    fn malformed_file(path: &std::path::Path, reason: String, message: String) -> Self {
        Self {
            abort_batch: true,
            error: CallToolResult::error_kind(
                ToolErrorKind::MalformedFile {
                    path: copperpour_sexp::paths::display_path(path),
                    reason,
                },
                message,
            ),
        }
    }
}

/// A field angle in `[0, 360)`, the form `rotate_schematic_component` writes.
/// KiCad 10.0.8 keeps `450` as written on resave, so this is our canonical
/// form, not KiCad's.
fn normalize_placement_rotation(degrees: f64) -> f64 {
    let wrapped = degrees.rem_euclid(360.0);
    if wrapped == 0.0 || wrapped == 360.0 {
        0.0
    } else {
        wrapped
    }
}

fn apply_field_placements(
    path: &std::path::Path,
    content: &mut String,
    reference: &str,
    args: &serde_json::Value,
    unit_count: usize,
) -> Result<FieldPlacementEdits, FieldPlacementFailure> {
    let placements = args["field_placements"].as_object();
    let mut plan = FieldPlacementEdits::default();
    if let Some(placements) = placements {
        let only_unit = field_placement_unit(args).map_err(FieldPlacementFailure::local)?;
        for (name, spec) in placements {
            let Some(spec) = spec.as_object() else {
                plan.errors
                    .push(format!("{name}: field_placements entries must be objects"));
                plan.error_kinds.push(EditErrorKind::InvalidArgument {
                    field: "field_placements",
                });
                continue;
            };
            let mut placement = FieldPlacement::default();
            let mut bad = false;
            for (key, slot) in [
                ("x", &mut placement.x),
                ("y", &mut placement.y),
                ("rotation", &mut placement.rotation),
            ] {
                match spec.get(key) {
                    None | Some(serde_json::Value::Null) => {}
                    Some(value) => match value.as_f64() {
                        Some(number) => {
                            *slot = Some(if key == "rotation" {
                                normalize_placement_rotation(number)
                            } else {
                                number
                            });
                        }
                        None => {
                            plan.errors.push(format!("{name}.{key}: expected a number"));
                            plan.error_kinds.push(EditErrorKind::InvalidArgument {
                                field: "field_placements",
                            });
                            bad = true;
                        }
                    },
                }
            }
            match spec.get("hide") {
                None | Some(serde_json::Value::Null) => {}
                Some(value) => match value.as_bool() {
                    Some(hide) => placement.hide = Some(hide),
                    None => {
                        plan.errors
                            .push(format!("{name}.hide: expected true or false"));
                        plan.error_kinds.push(EditErrorKind::InvalidArgument {
                            field: "field_placements",
                        });
                        bad = true;
                    }
                },
            }
            if bad {
                continue;
            }
            if placement.is_empty() {
                plan.errors.push(format!(
                    "{name}: field_placements entry sets nothing — give x, y, rotation or hide"
                ));
                plan.error_kinds.push(EditErrorKind::InvalidArgument {
                    field: "field_placements",
                });
                continue;
            }
            // A field's position is absolute, so one coordinate cannot be
            // right for several placements. Writing it to every unit would
            // stack the text of a quad gate's four Values on one point and
            // report success.
            if placement.moves_the_text() && only_unit.is_none() && unit_count > 1 {
                plan.errors.push(format!(
                    "{name}: '{reference}' has {} placed units, so a field position needs 'unit' — \
                     an absolute coordinate cannot be right for all of them",
                    unit_count
                ));
                plan.error_kinds
                    .push(EditErrorKind::InvalidArgument { field: "unit" });
                continue;
            }
            match set_property_placement(content, reference, name, &placement, only_unit) {
                Ok((updated, touched)) => {
                    *content = updated;
                    plan.applied.push((name.clone(), placement));
                    plan.unit = only_unit;
                    plan.changes
                        .push(format!("{name} placement ({touched} unit(s))"));
                }
                // A malformed file aborts the whole call. This handler edits
                // text and commits once, at the end, so a sibling edit that
                // succeeded would otherwise be written into a file already
                // found to be broken: the refusal would be reported while the
                // write went ahead anyway.
                Err(refusal @ PlacementRefusal::MalformedFile(_)) => {
                    let reason = format!("{name}: {}", refusal.reason());
                    let message = format!("No fields were updated on '{reference}': {reason}");
                    return Err(FieldPlacementFailure::malformed_file(path, reason, message));
                }
                Err(refusal) => {
                    plan.errors.push(format!("{name}: {}", refusal.reason()));
                    plan.error_kinds.push(EditErrorKind::StaleTarget);
                }
            }
        }
    }

    Ok(plan)
}

pub(crate) struct BatchFieldPlacements {
    pub(crate) changes: Vec<String>,
    pub(crate) errors: Vec<String>,
    pub(crate) intent: ComponentMutationIntent,
}

/// Plan with the same parser and placement writer as the single tool. Bind
/// UUIDs and body geometry before moving text so readback cannot bless a body
/// move merely because the requested field position was also written.
pub(crate) fn plan_batch_field_placements(
    path: &std::path::Path,
    content: &mut String,
    reference: &str,
    args: &serde_json::Value,
) -> Result<BatchFieldPlacements, FieldPlacementFailure> {
    parse_sexp(content).map_err(|error| {
        let reason = error.to_string();
        let message = format!("Schematic is malformed: {reason}");
        FieldPlacementFailure::malformed_file(path, reason, message)
    })?;
    let target = component_target_from_source(path, content, reference)
        .map_err(|error| FieldPlacementFailure::local(error.into_result()))?;
    let plan = apply_field_placements(path, content, reference, args, target.units.len())?;
    Ok(BatchFieldPlacements {
        changes: plan.changes,
        errors: plan.errors,
        intent: ComponentMutationIntent::new(target).with_field_placements(plan.applied, plan.unit),
    })
}

pub(crate) fn field_placement_unit(
    args: &serde_json::Value,
) -> Result<Option<u32>, CallToolResult> {
    match opt_u32(args, "unit")? {
        Some(0) => Err(crate::tools::invalid_arg(
            "unit",
            "expected a positive integer unit number",
        )),
        unit => Ok(unit),
    }
}

pub(crate) fn validate_field_placement_unit_scope(
    args: &serde_json::Value,
    whole_part_flag: Option<&str>,
) -> Result<(), CallToolResult> {
    if let Some(flag) = whole_part_flag {
        if !args["unit"].is_null()
            && args["field_placements"]
                .as_object()
                .is_none_or(|p| p.is_empty())
        {
            return Err(crate::tools::invalid_arg(
                "unit",
                &format!("selects the unit for field_placements only; {flag} is set on every placed unit"),
            ));
        }
    }
    Ok(())
}

/// What `edit_schematic_component` may change about where a field's text sits.
/// Every part is optional: an absent one leaves the file's value alone rather
/// than defaulting, so moving a field cannot silently unhide it.
#[derive(Debug, Default, Clone, Copy)]
pub(super) struct FieldPlacement {
    pub(super) x: Option<f64>,
    pub(super) y: Option<f64>,
    pub(super) rotation: Option<f64>,
    pub(super) hide: Option<bool>,
}

impl FieldPlacement {
    fn is_empty(&self) -> bool {
        self.x.is_none() && self.y.is_none() && self.rotation.is_none() && self.hide.is_none()
    }

    fn moves_the_text(&self) -> bool {
        self.x.is_some() || self.y.is_some()
    }
}

/// Does the committed file show the placement that was asked for?
///
/// Only the parts the caller set are compared; an absent one was never
/// requested, so whatever the file holds for it is correct. Extracted so the
/// comparison can be tested directly — a silent write failure cannot be
/// induced through the tool, and an untested guard is one nobody has watched
/// fail.
pub(super) fn placement_landed(seen: &serde_json::Value, placement: &FieldPlacement) -> bool {
    let agrees = |key: &str, want: Option<f64>| {
        want.is_none_or(|want| {
            seen[key]
                .as_f64()
                .is_some_and(|got| (got - want).abs() < 1e-6)
        })
    };
    agrees("x", placement.x)
        && agrees("y", placement.y)
        && agrees(
            "rotation",
            placement.rotation.map(normalize_placement_rotation),
        )
        && placement
            .hide
            .is_none_or(|want| seen["hide"].as_bool() == Some(want))
}

/// Move or hide one field's text on a placed symbol.
///
/// KiCad stores a field's position in absolute schematic millimetres on the
/// placement, not as an offset from the body, so a multi-unit part carries one
/// position per unit and a single coordinate cannot be right for all of them —
/// `only_unit` is how the caller says which placement it means.
///
/// The edit is scoped to the field's own `(property …)` block: a symbol's own
/// `(at …)` and its other fields' `(at …)` sit in the same instance block, and
/// a search that is not bounded by the property would move the body instead of
/// the text.
/// Why a field placement was refused, and how far the refusal reaches.
///
/// A malformed *file* makes the whole call unsafe: this handler edits text and
/// commits once at the end, so letting a sibling edit proceed would commit a
/// file we have already found to be broken. A target that simply was not there
/// is local to the field the caller named.
#[derive(Debug)]
pub(super) enum PlacementRefusal {
    /// The existing property or its placement is malformed. Aborts the call
    /// before anything is committed.
    MalformedFile(String),
    /// The named symbol, field or unit was not found. Refuses this field only.
    TargetMissing(String),
}

impl PlacementRefusal {
    fn reason(&self) -> &str {
        match self {
            Self::MalformedFile(reason) | Self::TargetMissing(reason) => reason,
        }
    }
}

/// Read a placed symbol block's own direct `(unit N)` child strictly.
///
/// Scanning the raw block text for `(unit ` matches quoted property values
/// placed earlier in the block, and defaulting parse failures to unit 1
/// silently reclassifies a corrupt unit as unit 1.
pub(super) fn symbol_unit_from_block(
    block: &str,
    reference: &str,
) -> Result<u32, PlacementRefusal> {
    let unit_children: Vec<_> = find_direct_child_blocks(block, "symbol")
        .into_iter()
        .filter(|&(child_start, child_end)| block_tag(&block[child_start..child_end]) == "unit")
        .collect();
    match unit_children.as_slice() {
        [(child_start, child_end)] => {
            let child = &block[*child_start..*child_end];
            let unit = parse_sexp(child)
                .ok()
                .and_then(|node| match node.children() {
                    Some([_, copperpour_sexp::SexpNode::Atom(token)]) => {
                        token.parse::<u32>().ok().filter(|&n| n > 0)
                    }
                    _ => None,
                });
            unit.ok_or_else(|| {
                PlacementRefusal::MalformedFile(format!(
                    "placed unit of '{reference}' has a malformed `{child}`"
                ))
            })
        }
        [] => Err(PlacementRefusal::MalformedFile(format!(
            "placed unit of '{reference}' carries no (unit …)"
        ))),
        _ => Err(PlacementRefusal::MalformedFile(format!(
            "placed unit of '{reference}' carries more than one (unit …)"
        ))),
    }
}

fn set_property_placement(
    content: &str,
    reference: &str,
    field: &str,
    placement: &FieldPlacement,
    only_unit: Option<u32>,
) -> Result<(String, usize), PlacementRefusal> {
    let blocks = find_all_symbol_instance_blocks(content, reference);
    if blocks.is_empty() {
        return Err(PlacementRefusal::TargetMissing(format!(
            "symbol '{reference}' not found in this schematic"
        )));
    }
    let escaped_field = escape_property_text(field);
    let field_search = format!(r#"(property "{escaped_field}" ""#);
    let mut edits = Vec::new();
    let mut touched = 0usize;

    for (start, end) in blocks {
        let block = &content[start..end];
        if let Some(wanted) = only_unit {
            let unit = symbol_unit_from_block(block, reference)?;
            if unit != wanted {
                continue;
            }
        }
        let Some(relative) = block.find(&field_search) else {
            return Err(PlacementRefusal::TargetMissing(format!(
                "'{reference}' has no '{field}' property on one of its placed units"
            )));
        };
        let (_prop_start, prop_end) =
            copperpour_sexp::writer::find_enclosing_block(content, "property", start + relative)
                .ok_or_else(|| {
                    PlacementRefusal::MalformedFile(format!(
                        "'{field}' property on '{reference}' is malformed"
                    ))
                })?;

        // Everything below searches for tokens *after* the property's value
        // string. A field value is arbitrary text and may contain "(at " or
        // "(hide " — searching the whole property block rewrites the value
        // instead of the placement, and the result is still valid
        // S-expression, so nothing downstream notices.
        let value_start = start + relative + field_search.len();
        let value_end = closing_quote(content, value_start).ok_or_else(|| {
            PlacementRefusal::MalformedFile(format!(
                "'{field}' property on '{reference}' is malformed"
            ))
        })?;
        let body_start = value_end + 1;
        let property = &content[body_start..prop_end];

        // The field's own (at x y [rot]); a property carries exactly one.
        let at_rel = property.find("(at ").ok_or_else(|| {
            PlacementRefusal::MalformedFile(format!(
                "'{field}' property on '{reference}' carries no (at …)"
            ))
        })?;
        let at_start = body_start + at_rel;
        let at_end = at_start
            + content[at_start..prop_end].find(')').ok_or_else(|| {
                PlacementRefusal::MalformedFile(format!(
                    "'{field}' property on '{reference}' is malformed"
                ))
            })?
            + 1;
        // Parse the existing placement *positionally* and strictly. Dropping
        // unparseable tokens and defaulting the gaps to zero silently shifts
        // the remaining ones left: `(at bad 20 90)` reads as x=20, y=90, and a
        // rotation-only request then commits `(at 20 90 NEW)` — a position the
        // caller never asked for and the file never held. A malformed
        // placement is refused here, before anything is committed, because a
        // wrong coordinate written into a schematic is silent.
        let raw = &content[at_start + "(at ".len()..at_end - 1];
        let tokens: Vec<&str> = raw.split_whitespace().collect();
        let coordinate = |index: usize, axis: &str| -> Result<f64, PlacementRefusal> {
            let token = tokens.get(index).ok_or_else(|| {
                PlacementRefusal::MalformedFile(format!(
                    "'{field}' placement on '{reference}' has no {axis} in `(at {raw})`"
                ))
            })?;
            match token.parse::<f64>() {
                Ok(value) if value.is_finite() => Ok(value),
                _ => Err(PlacementRefusal::MalformedFile(format!(
                    "'{field}' placement on '{reference}' has a non-numeric {axis} \
                     '{token}' in `(at {raw})`"
                ))),
            }
        };
        let current_x = coordinate(0, "x")?;
        let current_y = coordinate(1, "y")?;
        // KiCad writes `(at x y)` or `(at x y rotation)`; anything longer is
        // not a placement it produces, and rewriting it as three tokens would
        // drop whatever the extra ones carried.
        let current_rot = match tokens.len() {
            2 => 0.0,
            3 => coordinate(2, "rotation")?,
            count => {
                return Err(PlacementRefusal::MalformedFile(format!(
                    "'{field}' placement on '{reference}' has {count} values in \
                     `(at {raw})`; expected x, y and an optional rotation"
                )))
            }
        };
        if placement.x.is_some() || placement.y.is_some() || placement.rotation.is_some() {
            edits.push(SexpEdit::replace(
                at_start,
                at_end,
                format!(
                    "(at {} {} {})",
                    cse::types::fmt_f64(placement.x.unwrap_or(current_x)),
                    cse::types::fmt_f64(placement.y.unwrap_or(current_y)),
                    cse::types::fmt_f64(placement.rotation.unwrap_or(current_rot))
                ),
            ));
            for (from, to) in copperpour_sexp::writer::direct_children_with_tag(
                block,
                "symbol",
                "fields_autoplaced",
            ) {
                edits.push(SexpEdit::delete(start + from, start + to));
            }
        }

        if let Some(hide) = placement.hide {
            // Written as a direct child of the property. KiCad's own writer
            // nests the flag inside `(effects …)` on a placement and puts it
            // at property level in `lib_symbols`, but the two are equivalent
            // to it: at 10.0.6 a file in either form exports byte-identical
            // SVG, and its writer round-trips this one unchanged. The
            // readback accepts both, so a field KiCad hid reads as hidden.
            let existing_hide = property.find("(hide ").map(|rel| body_start + rel);
            match (hide, existing_hide) {
                (true, None) => {
                    // Match the indentation of the (at …) it follows, so the
                    // file keeps the shape its writer gave it (#210).
                    let line_start = content[..at_start].rfind('\n').map_or(0, |nl| nl + 1);
                    let indent = &content[line_start..at_start];
                    edits.push(SexpEdit::insert(at_end, format!("\n{indent}(hide yes)")));
                }
                (false, Some(at)) => {
                    let token_end =
                        at + content[at..prop_end].find(')').ok_or_else(|| {
                            PlacementRefusal::MalformedFile(format!(
                                "'{field}' hide flag on '{reference}' is malformed"
                            ))
                        })? + 1;
                    // Take the indentation before the token, and the newline
                    // too *only* if the token had the line to itself. Deleting
                    // back to the newline unconditionally eats whatever shares
                    // the line — on a file that writes `(at …) (hide yes)`
                    // together, that is the field's own position.
                    let bytes = content.as_bytes();
                    let mut from = at;
                    while from > 0 && matches!(bytes[from - 1], b' ' | b'\t') {
                        from -= 1;
                    }
                    if from == 0 || bytes[from - 1] != b'\n' {
                        // Shared line: leave the neighbours and their spacing.
                        from = at;
                    } else {
                        from -= 1;
                    }
                    edits.push(SexpEdit::replace(from, token_end, String::new()));
                }
                _ => {}
            }
        }
        touched += 1;
    }

    if touched == 0 {
        return Err(PlacementRefusal::TargetMissing(format!(
            "'{reference}' has no placed unit {}",
            only_unit.map(|u| u.to_string()).unwrap_or_default()
        )));
    }
    Ok((apply_edits(content.to_string(), edits), touched))
}

/// Rewrite the `(reference "…")` inside every unit's `(instances …)` block.
///
/// Returns the updated content and how many were rewritten. A multi-unit part
/// is placed once per unit and each placement carries its own instances block,
/// so a rename has to reach all of them or the units disagree about their own
/// designator.
fn rewrite_instance_references(
    content: &str,
    blocks: &[(usize, usize)],
    old_ref: &str,
    new_ref: &str,
) -> Result<(String, usize), String> {
    if blocks.is_empty() {
        return Err(format!("symbol '{old_ref}' not found after the rename"));
    }

    let search = format!(r#"(reference "{old_ref}")"#);
    let replacement = format!(r#"(reference "{new_ref}")"#);
    let mut edits = Vec::new();
    for (start, end) in blocks {
        let block = &content[*start..*end];
        let mut from = 0usize;
        while let Some(rel) = block[from..].find(&search) {
            let at = *start + from + rel;
            edits.push(SexpEdit::replace(
                at,
                at + search.len(),
                replacement.clone(),
            ));
            from += rel + search.len();
        }
    }
    if edits.is_empty() {
        return Err(format!(
            "'{new_ref}' has no (reference \"{old_ref}\") in its instances path — \
             the property was renamed but the netlist still reads the old designator"
        ));
    }
    let count = edits.len();
    Ok((apply_edits(content.to_string(), edits), count))
}

pub(super) async fn handle_edit_schematic_component(
    args: &serde_json::Value,
    _ctx: &ToolContext,
) -> anyhow::Result<CallToolResult> {
    let sch_path = get_path(args, "schematic")?;
    let reference = match require_str(args, "reference") {
        Ok(r) => r.to_string(),
        Err(e) => return Ok(e),
    };

    let requested_dnp = match args.get("dnp") {
        None | Some(serde_json::Value::Null) => None,
        Some(serde_json::Value::Bool(dnp)) => Some(*dnp),
        Some(_) => return Ok(crate::tools::invalid_arg("dnp", "must be a boolean")),
    };
    // Exclude from BOM and Exclude from board, KiCad's `(in_bom …)` and
    // `(on_board …)`, set as `dnp` is (#34).
    let mut requested_flags = Vec::new();
    for flag in ["in_bom", "on_board"] {
        match args.get(flag) {
            None | Some(serde_json::Value::Null) => {}
            Some(serde_json::Value::Bool(value)) => requested_flags.push((flag, *value)),
            Some(_) => return Ok(crate::tools::invalid_arg(flag, "must be a boolean")),
        }
    }
    // `unit` scopes field_placements only. Read beside `dnp`, `in_bom` or
    // `on_board` alone it looks like a request for one gate, and every unit
    // would change instead.
    let whole_part_flag = requested_dnp
        .map(|_| "dnp")
        .or_else(|| requested_flags.first().map(|(flag, _)| *flag));
    if let Err(error) = validate_field_placement_unit_scope(args, whole_part_flag) {
        return Ok(error);
    }
    // A property named DNP refuses the whole request, before anything is
    // read for writing: none of its other edits is applied either.
    if let Some(name) = args["fields"]
        .as_object()
        .and_then(|fields| fields.keys().find(|name| is_dnp_property(name)))
    {
        return Ok(crate::tools::invalid_arg(
            "fields",
            &dnp_property_refusal(name),
        ));
    }
    let remove_names = match remove_field_names(args) {
        Ok(names) => names,
        Err(error) => return Ok(error),
    };

    let mut content = read_consistent(&sch_path)?;
    let expected = content.clone();
    let target = match component_target_from_source(&sch_path, &expected, &reference) {
        Ok(target) => target,
        Err(error) => return Ok(error.into_result()),
    };
    // On a sheet placed more than once with its own designator per instance,
    // a rename would reach only the records that carry the old one, while
    // every other edit reaches all of them; which instance is meant is the
    // caller's to say, and `annotate_schematic` sets them per instance.
    if opt_str(args, "new_reference").is_some() {
        if let Some(unit) = target
            .units
            .iter()
            .find(|unit| unit.instance_references.len() > 1)
        {
            return Ok(ComponentDeleteTargetError::Ambiguous {
                target: format!("component {reference} sheet instances"),
                candidates: unit.instance_references.clone(),
            }
            .into_result());
        }
    }
    let mut changed = Vec::new();
    let mut expected_fields = BTreeMap::new();
    let mut applied_dnp = None;
    let (mut applied_in_bom, mut applied_on_board) = (None, None);
    let mut edit_reference = reference.as_str();
    let uuids = target.uuids();

    let mut errors: Vec<String> = Vec::new();
    let mut error_kinds: Vec<EditErrorKind> = Vec::new();
    // A macro rather than a closure: the body also needs `changed`/`errors`
    // between calls (the instances rewrite below, and the custom-field loop),
    // and a closure capturing them mutably would lock both for its lifetime.
    macro_rules! apply {
        ($field:expr, $new_val:expr) => {
            match set_property_value_in_blocks(
                &content,
                &symbol_blocks_by_uuid(&content, &uuids),
                edit_reference,
                $field,
                $new_val,
                false,
            ) {
                Ok((updated, counts)) => {
                    content = updated;
                    expected_fields.insert($field.to_owned(), $new_val.to_owned());
                    changed.push(format!(
                        "{} → {} ({} unit(s))",
                        $field, $new_val, counts.updated
                    ));
                }
                Err(why) => {
                    errors.push(format!("{}: {}", $field, why));
                    error_kinds.push(EditErrorKind::MalformedFile);
                }
            }
        };
    }

    // Every removal is planned on the file as read, before the call's first
    // edit, so a field that cannot be removed refuses the whole call instead of
    // leaving its other edits to be written. And before any rename: the blocks
    // that carry `reference` now are the units bound above, where after a
    // rename onto a designator already in use they would include another
    // part's.
    let mut removal_edits = Vec::new();
    let mut removed_fields = Vec::new();
    let mut absent_fields = Vec::new();
    for name in &remove_names {
        match plan_property_removal(
            &sch_path,
            &content,
            &symbol_blocks_by_uuid(&content, &uuids),
            &reference,
            name,
        ) {
            Ok((_, 0)) => absent_fields.push(name.clone()),
            Ok((edits, units)) => {
                removal_edits.extend(edits);
                removed_fields.push(name.clone());
                changed.push(format!("{name} removed ({units} unit(s))"));
            }
            Err(refusal) => return Ok(refusal),
        }
    }
    content = apply_edits(content, removal_edits);

    if let Some(new_ref) = opt_str(args, "new_reference") {
        let before_rename = content.clone();
        let before_changes = changed.len();
        let before_expected_fields = expected_fields.clone();
        apply!("Reference", new_ref);
        // A designator lives in TWO places. The (property "Reference" …) is
        // what renders; the (reference …) inside (instances …) is what KiCad
        // reads when it builds the netlist. Rewriting only the property leaves
        // the netlist on the old designator, so the rename appears to work in
        // eeschema and is ignored everywhere it matters (#157).
        match rewrite_instance_references(
            &content,
            &symbol_blocks_by_uuid(&content, &uuids),
            &reference,
            new_ref,
        ) {
            Ok((updated, count)) => {
                content = updated;
                edit_reference = new_ref;
                changed.push(format!("instances reference → {new_ref} ({count})"));
            }
            Err(why) => {
                content = before_rename;
                changed.truncate(before_changes);
                expected_fields = before_expected_fields;
                errors.push(format!("instances reference: {why}"));
                error_kinds.push(EditErrorKind::MalformedFile);
            }
        }
    }
    if let Some(val) = opt_str(args, "value") {
        apply!("Value", val);
    }
    if let Some(fp) = opt_str(args, "footprint") {
        apply!("Footprint", fp);
    }
    if let Some(ds) = opt_str(args, "datasheet") {
        apply!("Datasheet", ds);
    }
    // The native attribute, on every placed unit: KiCad keeps it per unit,
    // and with units that disagree its BOM marks the part DNP while a BOM
    // with DNP exclusion still lists it as fitted.
    if let Some(dnp) = requested_dnp {
        // Bound by UUID; a unit missed here fails the prospective readback,
        // before anything is committed.
        let blocks = symbol_blocks_by_uuid(&content, &uuids);
        match set_flag_in_blocks(&content, &blocks, &reference, "dnp", dnp) {
            Ok(updated) => {
                content = updated;
                applied_dnp = Some(dnp);
                changed.push(format!(
                    "dnp → {} ({} unit(s))",
                    if dnp { "yes" } else { "no" },
                    blocks.len()
                ));
            }
            // A unit whose (dnp …) cannot be rewritten is a malformed file.
            // Abort as a malformed field placement does, so no sibling edit
            // is written into it.
            Err(why) => {
                return Ok(ComponentDeleteTargetError::stale(
                    &sch_path,
                    format!("no fields were updated on '{reference}': dnp: {why}"),
                )
                .into_result())
            }
        }
    }
    // KiCad keeps these per unit too, and the board sync refuses a part
    // whose units disagree on them.
    for (flag, value) in requested_flags {
        let blocks = symbol_blocks_by_uuid(&content, &uuids);
        match set_flag_in_blocks(&content, &blocks, &reference, flag, value) {
            Ok(updated) => {
                content = updated;
                if flag == "in_bom" {
                    applied_in_bom = Some(value);
                } else {
                    applied_on_board = Some(value);
                }
                changed.push(format!(
                    "{flag} → {} ({} unit(s))",
                    if value { "yes" } else { "no" },
                    blocks.len()
                ));
            }
            Err(why) => {
                return Ok(ComponentDeleteTargetError::stale(
                    &sch_path,
                    format!("no fields were updated on '{reference}': {flag}: {why}"),
                )
                .into_result())
            }
        }
    }

    // `fields` has been in this tool's schema since it shipped and the handler
    // never read it, so custom properties were dropped and the call still
    // reported success (#158). An existing property is updated in place; a new
    // one is appended to the symbol block.
    let custom_fields = args["fields"].as_object();
    if let Some(fields) = custom_fields {
        for (name, value) in fields {
            let Some(value) = value.as_str() else {
                errors.push(format!("{name}: field values must be strings"));
                error_kinds.push(EditErrorKind::InvalidArgument { field: "fields" });
                continue;
            };
            if is_reserved_property(name) {
                errors.push(format!(
                    "{name}: set this through the '{}' parameter, not 'fields'",
                    name.to_ascii_lowercase()
                ));
                error_kinds.push(EditErrorKind::InvalidArgument { field: "fields" });
                continue;
            }
            match set_property_value_in_blocks(
                &content,
                &symbol_blocks_by_uuid(&content, &uuids),
                edit_reference,
                name,
                value,
                true,
            ) {
                Ok((updated, counts)) => {
                    content = updated;
                    expected_fields.insert(name.clone(), value.to_owned());
                    changed.push(format!(
                        "{name} → {value} ({} updated, {} added)",
                        counts.updated, counts.added
                    ));
                }
                Err(PropertyWriteError::AmbiguousVisibility(why)) => {
                    errors.push(format!("{name}: {why}"));
                    error_kinds.push(EditErrorKind::PlanBlocked);
                }
                Err(PropertyWriteError::Malformed(why)) => {
                    errors.push(format!("{name}: {why}"));
                    error_kinds.push(EditErrorKind::MalformedFile);
                }
            }
        }
    }

    // Field text placement. Separate from `fields`, which sets values: a
    // caller moving the Value text is not changing what it says, and
    // overloading one argument's value type would make the schema lie about
    // what it accepts.
    let placements = args["field_placements"].as_object();
    let plan = match apply_field_placements(
        &sch_path,
        &mut content,
        edit_reference,
        args,
        target.units.len(),
    ) {
        Ok(plan) => plan,
        Err(failure) => return Ok(failure.error),
    };
    changed.extend(plan.changes);
    errors.extend(plan.errors);
    error_kinds.extend(plan.error_kinds);
    let applied_placements = plan.applied;
    let placement_unit = plan.unit;

    // A request that changed nothing is a failure, not a success — silently
    // reporting `"changes": []` is what let the tab-indentation bug hide, and
    // what made a fields-only call report success while dropping every field
    // (#158): with `fields` unread, both `changed` and `errors` came back
    // empty and this guard never fired.
    if changed.is_empty()
        && (custom_fields.is_some_and(|f| !f.is_empty())
            || placements.is_some_and(|p| !p.is_empty()))
        && errors.is_empty()
    {
        let message = format!("No fields were updated on '{reference}'");
        return Ok(CallToolResult::error_kind(
            ToolErrorKind::PlanBlocked {
                operation: "edit_schematic_component".to_owned(),
                reasons: vec![message.clone()],
            },
            message,
        ));
    }
    if changed.is_empty() && !errors.is_empty() {
        let joined = errors.join("; ");
        let message = format!("No fields were updated on '{reference}': {joined}");
        let kind = if error_kinds.contains(&EditErrorKind::MalformedFile) {
            ToolErrorKind::MalformedFile {
                path: copperpour_sexp::paths::display_path(&sch_path),
                reason: joined,
            }
        } else if let Some(field) = error_kinds.iter().find_map(|kind| match kind {
            EditErrorKind::InvalidArgument { field } => Some(*field),
            _ => None,
        }) {
            ToolErrorKind::InvalidArgument {
                field: field.to_owned(),
                reason: joined,
            }
        } else if error_kinds.contains(&EditErrorKind::PlanBlocked) {
            ToolErrorKind::PlanBlocked {
                operation: "edit_schematic_component".to_owned(),
                reasons: vec![joined],
            }
        } else {
            ToolErrorKind::StaleTarget {
                target: format!("component {reference}"),
                reason: joined,
            }
        };
        return Ok(CallToolResult::error_kind(kind, message));
    }

    let intent = ComponentMutationIntent::new(
        target
            .with_fields(&expected_fields)
            .without_fields(&remove_names)
            .with_dnp(applied_dnp)
            .with_bom_and_board(applied_in_bom, applied_on_board),
    )
    .with_field_placements(applied_placements, placement_unit);
    let observed = if !changed.is_empty() {
        let item_ids = match target.item_ids() {
            Ok(item_ids) => item_ids,
            Err(error) => return Ok(error.into_result()),
        };
        let command = SchematicCommand::replace_items_from_document(
            &expected,
            &content,
            item_ids,
            format!("Edit {reference}"),
        )?;
        match commit_verified_component_mutation(
            &sch_path,
            &expected,
            &command,
            std::slice::from_ref(&intent),
            "edit_schematic_component",
        )? {
            Ok(mut observed) => observed.remove(0),
            Err(error) => return Ok(error),
        }
    } else {
        match load_component_mutation_readback(&sch_path, &intent.target)? {
            Ok(observed) => observed,
            Err(error) => return Ok(error),
        }
    };

    let mut result = json!({
        "reference": observed["reference"],
        "changes": changed
    });
    if observed["reference"] != reference {
        result["requested_reference"] = json!(reference);
    }
    copy_component_observation(&mut result, &observed);
    if !remove_names.is_empty() {
        result["removed_fields"] = json!(removed_fields);
        result["absent_fields"] = json!(absent_fields);
    }
    if !errors.is_empty() {
        result["errors"] = json!(errors);
    }
    Ok(CallToolResult::json(&result))
}
