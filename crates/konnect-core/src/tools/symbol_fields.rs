//! Library symbol fields: the ones `create_symbol` writes and
//! `set_symbol_properties` replaces on an existing `.kicad_sym` symbol (#892).

use crate::mcp::error::ToolErrorKind;
use crate::mcp::protocol::CallToolResult;
use crate::tool;
use crate::tools::footprint_metadata::{indent_before, quote_string};
use crate::tools::library::skip_sexp_token;
use crate::tools::sch_annotate::opt_string;
use crate::tools::{invalid_arg, opt_str_list, ToolContext, ToolDef};
use konnect_sexp::writer::{
    apply_edits, find_block_with_leading_whitespace, find_direct_child_blocks, read_consistent,
    transact_atomic, SexpEdit,
};
use konnect_sexp::{SexpError, SexpNode};
use serde_json::json;
use std::collections::BTreeMap;
use std::path::Path;

/// KiCad's mandatory symbol fields, in the order it writes them.
const MANDATORY_FIELDS: &[&str] = &[
    "Reference",
    "Value",
    "Footprint",
    "Datasheet",
    "Description",
];
const KEYWORDS: &str = "ki_keywords";
const FP_FILTERS: &str = "ki_fp_filters";
const LEGACY_DESCRIPTION: &str = "ki_description";
const MANDATORY_LIST: &str = "Reference, Value, Footprint, Datasheet, Description";
const KI_RESERVED: &str = "is a ki_* name, which KiCad keeps for its own symbol properties";

/// `schema` with the field arguments both tools share added to its properties.
pub(super) fn with_field_schema(mut schema: serde_json::Value) -> serde_json::Value {
    let properties = schema["properties"]
        .as_object_mut()
        .expect("a tool schema declares its properties");
    for (name, field) in [
        (
            "footprint",
            json!({
                "type": "string",
                "description": "Default footprint as 'Library:Footprint' (the Footprint field)"
            }),
        ),
        (
            "description",
            json!({
                "type": "string",
                "description": "Symbol description (the Description field)"
            }),
        ),
        (
            "keywords",
            json!({
                "type": "string",
                "description": "Space-separated search keywords (ki_keywords). An empty string writes none."
            }),
        ),
        (
            "fp_filters",
            json!({
                "type": "array",
                "description": "Footprint filter patterns such as 'SOIC*3.9x4.9mm*' (ki_fp_filters). An empty array writes none.",
                "items": { "type": "string" }
            }),
        ),
        (
            "fields",
            json!({
                "type": "object",
                "description": "Custom fields such as MPN, LCSC or Manufacturer, as name: value. New ones are written hidden, in name order. KiCad's mandatory field names and ki_* names, in any case, are refused.",
                "additionalProperties": { "type": "string" }
            }),
        ),
    ] {
        properties.insert(name.to_string(), field);
    }
    schema
}

/// The fields a call asked for, as KiCad property names and values. An empty
/// keywords or filter value means "no such property", as KiCad itself writes it.
#[derive(Debug, Clone, Default, PartialEq)]
pub(super) struct SymbolFields {
    pub(super) value: Option<String>,
    pub(super) footprint: Option<String>,
    pub(super) description: Option<String>,
    pub(super) keywords: Option<String>,
    pub(super) fp_filters: Option<String>,
    pub(super) custom: Vec<(String, String)>,
    /// Custom fields renamed, current name to new, and removed. Only
    /// `set_symbol_properties` takes these, and `value`.
    pub(super) renames: Vec<(String, String)>,
    pub(super) removals: Vec<String>,
}

impl SymbolFields {
    /// Every requested property, mandatory first, then custom, then `ki_*`:
    /// the order KiCad writes them.
    fn requested(&self) -> Vec<(&str, &str)> {
        let mut out = Vec::new();
        if let Some(value) = &self.value {
            out.push(("Value", value.as_str()));
        }
        if let Some(value) = &self.footprint {
            out.push(("Footprint", value.as_str()));
        }
        if let Some(value) = &self.description {
            out.push(("Description", value.as_str()));
        }
        out.extend(self.custom.iter().map(|(n, v)| (n.as_str(), v.as_str())));
        if let Some(value) = &self.keywords {
            out.push((KEYWORDS, value.as_str()));
        }
        if let Some(value) = &self.fp_filters {
            out.push((FP_FILTERS, value.as_str()));
        }
        out
    }

    fn is_empty(&self) -> bool {
        *self == Self::default()
    }

    /// The fields `create_symbol` writes after its Description: the custom
    /// ones, then keywords and filters unless empty.
    pub(super) fn created_after_description(&self) -> Vec<(&str, &str)> {
        self.requested()
            .into_iter()
            .filter(|(name, value)| {
                !MANDATORY_FIELDS.contains(name) && !absent_when_empty(name, value)
            })
            .collect()
    }
}

pub(super) fn parse_symbol_fields(
    args: &serde_json::Value,
) -> Result<SymbolFields, CallToolResult> {
    let keywords = opt_string(args, "keywords")?.map(|value| value.trim().to_string());
    let fp_filters = opt_str_list(args, "fp_filters")?;
    // KiCad splits the field on whitespace, so a pattern holding a space
    // would come back as two.
    if let Some(bad) = fp_filters
        .iter()
        .flatten()
        .find(|filter| filter.is_empty() || filter.chars().any(char::is_whitespace))
    {
        return Err(invalid_arg(
            "fp_filters",
            &format!("'{bad}' must be one non-empty pattern without whitespace"),
        ));
    }
    let mut custom = Vec::new();
    match args.get("fields") {
        None | Some(serde_json::Value::Null) => {}
        Some(value) => {
            let map = value
                .as_object()
                .ok_or_else(|| invalid_arg("fields", "must be an object of name: value"))?;
            for (name, value) in map {
                let field = format!("fields.{name}");
                if name.is_empty() || name.trim() != name {
                    return Err(invalid_arg(
                        &field,
                        "a field name must be non-empty, without surrounding whitespace",
                    ));
                }
                if let Some(owner) = reserved_owner(name) {
                    return Err(invalid_arg(
                        &field,
                        &format!("'{name}' is not a custom field; {owner}"),
                    ));
                }
                let value = value
                    .as_str()
                    .ok_or_else(|| invalid_arg(&field, "must be a string"))?;
                custom.push((name.clone(), value.to_string()));
            }
        }
    }
    // Name order, whatever order the JSON map keeps its keys in.
    custom.sort();
    Ok(SymbolFields {
        footprint: opt_string(args, "footprint")?,
        description: opt_string(args, "description")?,
        keywords,
        fp_filters: fp_filters.map(|filters| filters.join(" ")),
        custom,
        ..SymbolFields::default()
    })
}

/// Why a custom field may not take `name`, or `None` when it may.
fn reserved_owner(name: &str) -> Option<&'static str> {
    if ["Value", "Footprint", "Description"]
        .iter()
        .any(|field| field.eq_ignore_ascii_case(name))
    {
        Some("use the value, footprint or description argument")
    } else if MANDATORY_FIELDS
        .iter()
        .any(|field| field.eq_ignore_ascii_case(name))
    {
        Some("Reference and Datasheet are mandatory fields")
    } else if name.to_ascii_lowercase().starts_with("ki_") {
        Some("ki_* names are reserved by KiCad; use keywords or fp_filters")
    } else {
        None
    }
}

/// The arguments only `set_symbol_properties` takes, checked in full before
/// the library is read. A name has one part in a call: it is set, renamed,
/// or removed.
fn parse_symbol_edits(
    args: &serde_json::Value,
    fields: &mut SymbolFields,
) -> Result<(), CallToolResult> {
    fields.value = opt_string(args, "value")?;
    fields.renames = parse_rename_fields(args, fields)?;
    fields.removals = parse_remove_fields(args, fields)?;
    Ok(())
}

fn parse_rename_fields(
    args: &serde_json::Value,
    fields: &SymbolFields,
) -> Result<Vec<(String, String)>, CallToolResult> {
    // An explicit null is refused like any other non-object, as the schema
    // refuses it.
    let map = match args.get("rename_fields") {
        None => return Ok(Vec::new()),
        Some(value) => value.as_object().ok_or_else(|| {
            invalid_arg(
                "rename_fields",
                "must be an object of current name: new name",
            )
        })?,
    };
    let mut renames = Vec::new();
    for (old, new) in map {
        let field = format!("rename_fields.{old}");
        let new = new
            .as_str()
            .ok_or_else(|| invalid_arg(&field, "must be a string"))?;
        if new.is_empty() || new.trim() != new {
            return Err(invalid_arg(
                &field,
                "a field name must be non-empty, without surrounding whitespace",
            ));
        }
        let refusal = if is_mandatory(old) {
            Some(format!(
                "'{old}' is one of KiCad's mandatory symbol fields ({MANDATORY_LIST}), whose names \
                 cannot change"
            ))
        } else if is_ki_name(old) {
            Some(format!("'{old}' {KI_RESERVED}"))
        } else if is_mandatory(new) {
            Some(format!(
                "'{new}' is one of KiCad's mandatory symbol fields ({MANDATORY_LIST}), so a custom \
                 field cannot take that name"
            ))
        } else if is_ki_name(new) {
            Some(format!("'{new}' {KI_RESERVED}"))
        } else if is_generated_name(new) {
            // SCH_FIELD::SetName marks such a field generated, and its text is
            // then its name, so the value would be lost.
            Some(format!(
                "'{new}' has the form of a KiCad generated field, whose text KiCad replaces with \
                 its name on load"
            ))
        } else {
            fields
                .custom
                .iter()
                .find(|(name, _)| name == old || name == new)
                .map(|(name, _)| {
                    format!("'{name}' is also named in fields; a call cannot both set and rename a field")
                })
        };
        if let Some(reason) = refusal {
            return Err(invalid_arg(&field, &reason));
        }
        renames.push((old.clone(), new.to_string()));
    }
    // Current-name order, whatever order the JSON map keeps its keys in.
    renames.sort();
    for (index, (old, new)) in renames.iter().enumerate() {
        let field = format!("rename_fields.{old}");
        if let Some((other, _)) = renames[..index].iter().find(|(_, earlier)| earlier == new) {
            return Err(invalid_arg(
                &field,
                &format!("'{new}' is also the new name of '{other}'; a symbol carries one field per name"),
            ));
        }
        if new != old && renames.iter().any(|(current, _)| current == new) {
            return Err(invalid_arg(
                &field,
                &format!(
                    "'{new}' is also a current name in rename_fields; a chain or swap of names \
                     takes one call per step"
                ),
            ));
        }
    }
    Ok(renames)
}

/// `remove_fields`, checked in full before the library is read. A repeated
/// name is removed once.
fn parse_remove_fields(
    args: &serde_json::Value,
    fields: &SymbolFields,
) -> Result<Vec<String>, CallToolResult> {
    // An explicit null is refused like any other non-array, as the schema
    // refuses it.
    let entries = match args.get("remove_fields") {
        None => return Ok(Vec::new()),
        Some(serde_json::Value::Array(entries)) => entries,
        Some(_) => {
            return Err(invalid_arg(
                "remove_fields",
                "expected an array of field names",
            ))
        }
    };
    let mut names: Vec<String> = Vec::new();
    for entry in entries {
        let Some(name) = entry.as_str().filter(|name| !name.is_empty()) else {
            return Err(invalid_arg(
                "remove_fields",
                "every entry must be a non-empty field name",
            ));
        };
        let refusal = if is_mandatory(name) {
            Some(format!(
                "'{name}' is one of KiCad's mandatory symbol fields ({MANDATORY_LIST}), which \
                 cannot be removed"
            ))
        } else if is_ki_name(name) {
            Some(format!(
                "'{name}' {KI_RESERVED}; an empty keywords or fp_filters removes ki_keywords or \
                 ki_fp_filters"
            ))
        } else if fields.custom.iter().any(|(set, _)| set == name) {
            Some(format!(
                "'{name}' is also named in fields; a call cannot both change and remove a field"
            ))
        } else if fields
            .renames
            .iter()
            .any(|(old, new)| old == name || new == name)
        {
            Some(format!(
                "'{name}' is also named in rename_fields; a call cannot both change and remove a field"
            ))
        } else {
            None
        };
        if let Some(reason) = refusal {
            return Err(invalid_arg("remove_fields", &reason));
        }
        if !names.iter().any(|seen| seen == name) {
            names.push(name.to_owned());
        }
    }
    Ok(names)
}

/// A mandatory field's name, which KiCad reads in any case.
fn is_mandatory(name: &str) -> bool {
    MANDATORY_FIELDS
        .iter()
        .any(|field| field.eq_ignore_ascii_case(name))
}

fn is_ki_name(name: &str) -> bool {
    name.to_ascii_lowercase().starts_with("ki_")
}

/// KiCad's generated-field pattern, `^\$\{\w*\}$`.
fn is_generated_name(name: &str) -> bool {
    name.strip_prefix("${")
        .and_then(|rest| rest.strip_suffix('}'))
        .is_some_and(|word| word.chars().all(|c| c.is_alphanumeric() || c == '_'))
}

pub(super) fn tool() -> ToolDef {
    tool!(
        "set_symbol_properties",
        "Set the Value, Footprint, Description, keywords, footprint filters or custom fields \
         (MPN, LCSC, ...) of an existing library symbol, and rename or remove its custom \
         fields. Only the supplied fields change; an existing or renamed field keeps its position \
         and visibility. Reports each changed field's previous and current value.",
        with_field_schema(json!({
            "type": "object",
            "properties": {
                "library_path": { "type": "string", "description": "Path to an existing .kicad_sym library file" },
                "symbol_name": { "type": "string", "description": "Name of the symbol to change" },
                "value": { "type": "string", "description": "Symbol value (the Value field)" },
                "rename_fields": {
                    "type": "object",
                    "description": "Custom fields to rename, as current name: new name. A renamed field keeps its value, position, visibility and text effects. Names match whole and case-sensitively, as KiCad's custom field names do; a new name the symbol already carries is refused.",
                    "additionalProperties": { "type": "string" }
                },
                "remove_fields": {
                    "type": "array",
                    "description": "Custom field names to delete from the symbol. KiCad's mandatory fields (Reference, Value, Footprint, Datasheet, Description) and ki_* names cannot be removed. A name the symbol does not carry is reported in absent_fields.",
                    "items": { "type": "string" }
                }
            },
            "required": ["library_path", "symbol_name"],
            "additionalProperties": false
        })),
        |args, ctx| async move { handle_set_symbol_properties(args, ctx).await }
    )
}

async fn handle_set_symbol_properties(
    args: &serde_json::Value,
    _ctx: &ToolContext,
) -> anyhow::Result<CallToolResult> {
    let path = super::get_path(args, "library_path")?;
    let symbol = match super::require_str(args, "symbol_name") {
        Ok(symbol) => symbol,
        Err(error) => return Ok(error),
    };
    if path.extension().and_then(|extension| extension.to_str()) != Some("kicad_sym") {
        return Ok(unusable(&path, "it does not end in .kicad_sym"));
    }
    let mut fields = match parse_symbol_fields(args) {
        Ok(fields) => fields,
        Err(error) => return Ok(error),
    };
    if let Err(error) = parse_symbol_edits(args, &mut fields) {
        return Ok(error);
    }
    if fields.is_empty() {
        return Ok(invalid_arg(
            "symbol_properties",
            "supply at least one of value, footprint, description, keywords, fp_filters, \
             fields, rename_fields or remove_fields",
        ));
    }
    let source = match read_consistent(&path) {
        Ok(source) => source,
        Err(SexpError::Io(error)) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok(CallToolResult::error_kind(
                ToolErrorKind::FileNotFound {
                    path: path.display().to_string(),
                },
                format!("Symbol library not found: {}", path.display()),
            ));
        }
        Err(error) => return Err(error.into()),
    };
    let PreparedEdit {
        replacement,
        changes,
    } = match prepare_edit(&path, &source, symbol, &fields) {
        Ok(edit) => edit,
        Err(error) => return Ok(error),
    };

    if replacement != source {
        let check = |text: &str| readback_mismatch(text, &source, symbol, &fields);
        let refusal = write_checked(&path, &source, replacement, "set_symbol_properties", check)?;
        if let Some(refusal) = refusal {
            return Ok(refusal);
        }
    }

    let mut body = json!({
        "success": true,
        "library_path": path.display().to_string(),
        "symbol": symbol,
        "changes": changes
    });
    if !fields.removals.is_empty() {
        // Only a removal puts a remove_fields name into `changes`, since each
        // name has one part in a call.
        let (removed, absent): (Vec<&String>, Vec<&String>) = fields
            .removals
            .iter()
            .partition(|name| changes.contains_key(name.as_str()));
        body["removed_fields"] = json!(removed);
        body["absent_fields"] = json!(absent);
    }
    Ok(CallToolResult::json(&body))
}

/// A library that was named but cannot be edited. Like a missing file, that is
/// not an argument error (DEV.md, "Structured Errors").
pub(super) fn unusable(path: &Path, reason: &str) -> CallToolResult {
    handler_error(format!(
        "Cannot edit symbol library '{}': {reason}",
        path.display()
    ))
}

fn handler_error(message: String) -> CallToolResult {
    CallToolResult::error_kind(
        ToolErrorKind::HandlerError {
            reason: message.clone(),
        },
        message,
    )
}

/// Replaces the library at `path` with `replacement` if it still holds
/// `source`, then reads it back. `check` says why a text is not the planned
/// edit; it is asked of `replacement` before the write and of the file after.
/// `None` once the edit is written and read back.
pub(super) fn write_checked(
    path: &Path,
    source: &str,
    replacement: String,
    operation: &str,
    check: impl Fn(&str) -> Option<String>,
) -> anyhow::Result<Option<CallToolResult>> {
    // A planning mistake is refused here, with nothing written.
    if let Some(reason) = check(&replacement) {
        return Ok(Some(handler_error(format!(
            "{operation} wrote nothing: its planned edit failed its own check ({reason})"
        ))));
    }
    #[cfg(test)]
    write_hook::run(path);
    let written = transact_atomic(path, |current| {
        Ok(if current == source {
            (replacement, true)
        } else {
            (current.to_owned(), false)
        })
    });
    match written {
        Ok(true) => {}
        Ok(false) => {
            return Ok(Some(CallToolResult::error_kind(
                ToolErrorKind::Conflict {
                    paths: vec![path.display().to_string()],
                },
                format!(
                    "Symbol library '{}' changed after Copperpour read it, so {operation} wrote \
                     nothing. Reload it and retry.",
                    path.display()
                ),
            )))
        }
        Err(SexpError::Conflict { .. }) => {
            return Ok(Some(uncertain(
                path,
                operation,
                "the library no longer held the edit when read back after the write",
            )))
        }
        Err(error) => return Err(error.into()),
    }
    let reason = match read_consistent(path) {
        Ok(written) => check(&written),
        Err(error) => Some(format!("it could not be read back: {error}")),
    };
    Ok(reason.map(|reason| uncertain(path, operation, reason)))
}

fn uncertain(path: &Path, operation: &str, reason: impl Into<String>) -> CallToolResult {
    let path = path.display().to_string();
    let reason = reason.into();
    CallToolResult::error_kind(
        ToolErrorKind::MutationOutcomeUncertain {
            operation: operation.to_string(),
            path: path.clone(),
            reason: reason.clone(),
        },
        format!(
            "{operation} replaced library '{path}', but {reason}. Another program may have \
             saved it. Reload and inspect the library before retrying."
        ),
    )
}

#[derive(Debug)]
struct PreparedEdit {
    replacement: String,
    /// `{property: {"previous": …, "current": …}}` for each changed property;
    /// an absent property reads as null.
    changes: serde_json::Map<String, serde_json::Value>,
}

/// One `(property "NAME" "VALUE" …)` child of the target symbol.
struct Property {
    name: String,
    value: String,
    block: (usize, usize),
    name_span: (usize, usize),
    value_span: (usize, usize),
}

/// The property KiCad reads `name` from. A KiCad 6/7 library keeps the
/// description in `ki_description`, and the later of the two wins on load.
fn field<'a>(properties: &'a [Property], name: &str) -> Option<&'a Property> {
    properties.iter().rfind(|property| {
        property.name == name || (name == "Description" && property.name == LEGACY_DESCRIPTION)
    })
}

/// The property named exactly `name`, as KiCad compares custom field names.
/// Renames and removals use this rather than [`field`], so the KiCad 6/7
/// `ki_description` alias cannot reach them.
fn exact<'a>(properties: &'a [Property], name: &str) -> Option<&'a Property> {
    properties.iter().find(|property| property.name == name)
}

/// KiCad writes no keywords or filter property when it has none.
fn absent_when_empty(name: &str, value: &str) -> bool {
    value.is_empty() && matches!(name, KEYWORDS | FP_FILTERS)
}

/// Where a property sits in KiCad's write order: mandatory fields, then
/// custom fields, then keywords, then footprint filters.
fn rank(name: &str) -> usize {
    match name {
        KEYWORDS => MANDATORY_FIELDS.len() + 1,
        FP_FILTERS => MANDATORY_FIELDS.len() + 2,
        _ => MANDATORY_FIELDS
            .iter()
            .position(|field| *field == name)
            .unwrap_or(MANDATORY_FIELDS.len()),
    }
}

fn prepare_edit(
    path: &Path,
    source: &str,
    symbol: &str,
    fields: &SymbolFields,
) -> Result<PreparedEdit, CallToolResult> {
    let root = source.len() - source.trim_start().len();
    if header(source, root).map(|(head, _)| head) != Some("kicad_symbol_lib") {
        return Err(unusable(path, "it is not a (kicad_symbol_lib ...) file"));
    }
    let (start, end) = symbol_block(source, symbol)?;
    let properties =
        symbol_properties(source, start, end).map_err(|reason| unusable(path, &reason))?;
    let indent = properties
        .first()
        .and_then(|property| indent_before(source, property.block.0))
        .ok_or_else(|| unusable(path, &format!("symbol '{symbol}' has no fields")))?;
    let unit = if indent.contains('\t') { "\t" } else { "  " };

    // Every symbol KiCad writes carries its Value; one that does not would get
    // a second, hidden one from the insert path below.
    if fields.value.is_some() && exact(&properties, "Value").is_none() {
        let folded = properties
            .iter()
            .find(|property| property.name.eq_ignore_ascii_case("Value"))
            .map(|property| format!("; it has '{}', which KiCad reads as Value", property.name))
            .unwrap_or_default();
        return Err(unusable(
            path,
            &format!("symbol '{symbol}' has no Value field{folded}"),
        ));
    }

    let mut deletions = Vec::new();
    let mut replacements = Vec::new();
    let mut insertions: Vec<(&str, &str)> = Vec::new();
    let mut changes = serde_json::Map::new();
    for (name, value) in fields.requested() {
        let existing = field(&properties, name);
        let current = match (existing, absent_when_empty(name, value)) {
            (Some(property), true) => {
                let range = find_block_with_leading_whitespace(source, property.block.0)
                    .ok_or_else(|| unusable(path, "it holds an unbalanced field"))?;
                deletions.push(SexpEdit::delete(range.0, range.1));
                None
            }
            (Some(property), false) if property.value != value => {
                replacements.push(SexpEdit::replace(
                    property.value_span.0,
                    property.value_span.1,
                    quote_string(value),
                ));
                Some(value)
            }
            (None, false) => {
                insertions.push((name, value));
                Some(value)
            }
            _ => continue,
        };
        changes.insert(
            name.to_string(),
            json!({ "previous": existing.map(|p| p.value.as_str()), "current": current }),
        );
    }

    // A rename replaces the name token alone, as KiCad's Symbol Properties
    // dialog does, so the field keeps its value, its place and its look.
    for (old, new) in &fields.renames {
        let argument = format!("rename_fields.{old}");
        let Some(property) = exact(&properties, old) else {
            let mut reason = format!("symbol '{symbol}' has no field '{old}'");
            if exact(&properties, new).is_some() {
                reason.push_str(&format!("; it has a field '{new}'"));
            }
            for variant in properties.iter().filter(|property| {
                property.name != *old
                    && property.name != *new
                    && property.name.eq_ignore_ascii_case(old)
            }) {
                reason.push_str(&format!(
                    "; it has '{}', which KiCad treats as a different field",
                    variant.name
                ));
            }
            return Err(invalid_arg(&argument, &reason));
        };
        if old == new {
            continue;
        }
        if exact(&properties, new).is_some() {
            return Err(invalid_arg(
                &argument,
                &format!(
                    "symbol '{symbol}' already has a field '{new}', and KiCad refuses a field \
                     name already in use"
                ),
            ));
        }
        replacements.push(SexpEdit::replace(
            property.name_span.0,
            property.name_span.1,
            quote_string(new),
        ));
        changes.insert(
            old.clone(),
            json!({ "previous": property.value, "current": null }),
        );
        changes.insert(
            new.clone(),
            json!({ "previous": null, "current": property.value }),
        );
    }
    for name in &fields.removals {
        let Some(property) = exact(&properties, name) else {
            continue;
        };
        let range = find_block_with_leading_whitespace(source, property.block.0)
            .ok_or_else(|| unusable(path, "it holds an unbalanced field"))?;
        deletions.push(SexpEdit::delete(range.0, range.1));
        changes.insert(
            name.clone(),
            json!({ "previous": property.value, "current": null }),
        );
    }

    // A new property goes after the last one KiCad writes before it. Inserts
    // at one offset are joined, because `apply_edits` would apply them in
    // reverse. A deletion starts where the property before it ends, so it sits
    // ahead of the insertions in the edit list: at a shared offset the delete
    // runs first and cannot swallow the inserted text.
    let mut by_anchor: BTreeMap<usize, String> = BTreeMap::new();
    for (name, value) in insertions {
        let anchor = properties
            .iter()
            .rfind(|p| rank(&p.name) <= rank(name))
            .unwrap_or(&properties[0])
            .block
            .1;
        by_anchor.entry(anchor).or_default().push_str(&format!(
            "\n{indent}{}",
            hidden_property(name, value, &indent, unit)
        ));
    }
    let mut edits = deletions;
    edits.extend(replacements);
    edits.extend(
        by_anchor
            .into_iter()
            .map(|(offset, text)| SexpEdit::insert(offset, text)),
    );
    Ok(PreparedEdit {
        replacement: apply_edits(source.to_string(), edits),
        changes,
    })
}

/// The top-level `(symbol "NAME" …)` block of the library.
pub(super) fn symbol_block(source: &str, symbol: &str) -> Result<(usize, usize), CallToolResult> {
    let matches: Vec<(usize, usize)> = find_direct_child_blocks(source, "kicad_symbol_lib")
        .into_iter()
        .filter(|(start, _)| {
            header(source, *start).is_some_and(|(head, name)| {
                head == "symbol"
                    && konnect_sexp::parse_sexp(name).is_ok_and(|n| n.as_str() == Some(symbol))
            })
        })
        .collect();
    match matches.as_slice() {
        [block] => Ok(*block),
        [] => Err(invalid_arg(
            "symbol_name",
            &format!("no symbol named '{symbol}' in the library"),
        )),
        _ => Err(CallToolResult::error_kind(
            ToolErrorKind::AmbiguousTarget {
                target: symbol.to_string(),
                candidates: matches
                    .iter()
                    .map(|(start, _)| format!("byte offset {start}"))
                    .collect(),
            },
            format!(
                "The library defines {} symbols named '{symbol}'; Copperpour cannot tell which \
                 one to change. Remove the duplicates first.",
                matches.len()
            ),
        )),
    }
}

/// The direct `property` children of the symbol block at `start..end`, or
/// why the block cannot be edited.
fn symbol_properties(source: &str, start: usize, end: usize) -> Result<Vec<Property>, String> {
    let mut properties: Vec<Property> = Vec::new();
    for (child_start, child_end) in find_direct_child_blocks(&source[start..end], "symbol") {
        let block = (start + child_start, start + child_end);
        if header(source, block.0).is_none_or(|(head, _)| head != "property") {
            continue;
        }
        let node = konnect_sexp::parse_sexp(&source[block.0..block.1])
            .map_err(|_| "it holds a field that does not parse".to_string())?;
        // KiCad writes a private field as `(property private "NAME" "VALUE" …)`.
        let private = matches!(node.get(1), Some(SexpNode::Atom(atom)) if atom == "private");
        let first = 1 + usize::from(private);
        let (Some(name), Some(value), Some((name_span, value_span))) = (
            node.get(first).and_then(|n| n.as_str()),
            node.get(first + 1).and_then(|v| v.as_str()),
            name_and_value_spans(source, block.0, private),
        ) else {
            return Err("it holds a field without a quoted name and value".into());
        };
        if properties.iter().any(|property| property.name == name) {
            return Err(format!("the symbol has two '{name}' fields"));
        }
        properties.push(Property {
            name: name.to_string(),
            value: value.to_string(),
            block,
            name_span,
            value_span,
        });
    }
    Ok(properties)
}

/// The head and first argument of the block opening at `start`, as written.
pub(super) fn header(source: &str, start: usize) -> Option<(&str, &str)> {
    let (head_start, head_end) = skip_sexp_token(source, start + 1)?;
    let (name_start, name_end) = skip_sexp_token(source, head_end)?;
    Some((&source[head_start..head_end], &source[name_start..name_end]))
}

/// The spans of the name and value strings in the `(property "NAME" "VALUE"`
/// at `start`, past a `private` atom when there is one.
fn name_and_value_spans(
    source: &str,
    start: usize,
    private: bool,
) -> Option<((usize, usize), (usize, usize))> {
    let (_, mut name_from) = skip_sexp_token(source, start + 1)?;
    if private {
        name_from = skip_sexp_token(source, name_from)?.1;
    }
    let name = skip_sexp_token(source, name_from)?;
    Some((name, skip_sexp_token(source, name.1)?))
}

/// A hidden field in KiCad's own layout, so a resave leaves it as written.
fn hidden_property(name: &str, value: &str, indent: &str, unit: &str) -> String {
    let [i1, i2, i3] = [1, 2, 3].map(|depth| format!("{indent}{}", unit.repeat(depth)));
    format!(
        "(property {} {}\n{i1}(at 0 0 0)\n{i1}(show_name no)\n{i1}(do_not_autoplace no)\n\
         {i1}(hide yes)\n{i1}(effects\n{i2}(font\n{i3}(size 1.27 1.27)\n{i2})\n{i1})\n{indent})",
        quote_string(name),
        quote_string(value)
    )
}

/// Why `written` does not hold every requested field of `symbol`, or `None`
/// when it does. A removed or renamed-away name must be gone, and a renamed
/// field must be its block in `before` with only the name changed.
fn readback_mismatch(
    written: &str,
    before: &str,
    symbol: &str,
    fields: &SymbolFields,
) -> Option<String> {
    let Ok((start, end)) = symbol_block(written, symbol) else {
        return Some(format!("symbol '{symbol}' is no longer in the library"));
    };
    let Ok(properties) = symbol_properties(written, start, end) else {
        return Some(format!("symbol '{symbol}' no longer has readable fields"));
    };
    let requested = fields.requested().into_iter().find_map(|(name, value)| {
        let found = field(&properties, name).map(|property| property.value.as_str());
        let expected = (!absent_when_empty(name, value)).then_some(value);
        (found != expected)
            .then(|| format!("field '{name}' reads {found:?} instead of {expected:?}"))
    });
    let removed = || {
        fields
            .removals
            .iter()
            .find(|name| exact(&properties, name).is_some())
            .map(|name| format!("field '{name}' is still present after its removal"))
    };
    let renamed = || {
        let original = symbol_block(before, symbol)
            .ok()
            .and_then(|(start, end)| symbol_properties(before, start, end).ok())?;
        fields
            .renames
            .iter()
            .filter(|(old, new)| old != new)
            .find_map(|(old, new)| {
                if exact(&properties, old).is_some() {
                    return Some(format!("field '{old}' is still present after its rename"));
                }
                let was = exact(&original, old)?;
                let expected = format!(
                    "{}{}{}",
                    &before[was.block.0..was.name_span.0],
                    quote_string(new),
                    &before[was.name_span.1..was.block.1]
                );
                let found = exact(&properties, new).map(|p| &written[p.block.0..p.block.1]);
                (found != Some(expected.as_str()))
                    .then(|| format!("field '{new}' is not '{old}' renamed"))
            })
    };
    requested.or_else(removed).or_else(renamed)
}

/// A test-only seam between the read and the write: the next call writes
/// the bytes set here first, as an intervening save would.
#[cfg(test)]
pub(super) mod write_hook {
    use std::cell::RefCell;
    use std::path::Path;

    thread_local! {
        static SAVE: RefCell<Option<String>> = const { RefCell::new(None) };
    }

    pub(in crate::tools) fn set(content: String) {
        SAVE.with(|slot| *slot.borrow_mut() = Some(content));
    }

    pub(super) fn run(path: &Path) {
        if let Some(content) = SAVE.with(|slot| slot.borrow_mut().take()) {
            std::fs::write(path, content).unwrap();
        }
    }
}

#[cfg(test)]
pub(super) mod tests {
    use super::*;
    use crate::mcp::handler::McpHandler;
    use crate::tools::ServerConfig;

    /// KiCad 10.0.6's resave of two `create_symbol` symbols; see the README.
    const FIXTURE: &str = include_str!("../../tests/fixtures/symbol_fields_kicad10.kicad_sym");
    /// KiCad's resave of FIXTURE after the README's two edits.
    const EDITED: &str =
        include_str!("../../tests/fixtures/symbol_fields_kicad10.edited.kicad_sym");

    async fn call(
        handler: &McpHandler,
        name: &str,
        arguments: serde_json::Value,
    ) -> serde_json::Value {
        let response = handler
            .handle_message(json!({
                "jsonrpc": "2.0", "id": 892, "method": "tools/call",
                "params": {"name": name, "arguments": arguments}
            }))
            .await
            .unwrap()
            .result
            .unwrap();
        let mut body: serde_json::Value =
            serde_json::from_str(response["content"][0]["text"].as_str().unwrap()).unwrap();
        body["is_error"] = response["isError"].clone();
        body
    }

    async fn handler() -> McpHandler {
        McpHandler::new(ServerConfig {
            eager_toolsets: true,
            ..Default::default()
        })
        .await
        .unwrap()
    }

    /// `(name, value)` of every field of `symbol`, in file order.
    fn fields_of(source: &str, symbol: &str) -> Vec<(String, String)> {
        let (start, end) = symbol_block(source, symbol).unwrap();
        symbol_properties(source, start, end)
            .unwrap()
            .into_iter()
            .map(|property| (property.name, property.value))
            .collect()
    }

    fn library_with(source: &str) -> (tempfile::TempDir, std::path::PathBuf) {
        library_named("fields.kicad_sym", source)
    }

    fn library_named(name: &str, source: &str) -> (tempfile::TempDir, std::path::PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(name);
        std::fs::write(&path, source).unwrap();
        (dir, path)
    }

    fn bare_edit(path: &Path) -> serde_json::Value {
        json!({
            "library_path": path, "symbol_name": "FIELDS_BARE",
            "footprint": "Package_DFN_QFN:QFN-20-1EP_3x3mm_P0.4mm_EP1.65x1.65mm",
            "description": "USB 2.0 to \"UART\\JTAG\" bridge",
            "keywords": "usb uart jtag",
            "fp_filters": ["QFN*3x3mm*P0.4mm*", "TSSOP*"],
            "fields": {"Manufacturer": "WCH", "MPN": "CH347F"}
        })
    }

    #[tokio::test]
    async fn edits_write_what_kicad_resaves() {
        let handler = handler().await;
        let (_dir, path) = library_with(FIXTURE);

        let bare = call(&handler, "set_symbol_properties", bare_edit(&path)).await;
        assert_ne!(bare["is_error"], true, "{bare}");
        assert_eq!(
            bare["changes"]["Footprint"],
            json!({"previous": "", "current": "Package_DFN_QFN:QFN-20-1EP_3x3mm_P0.4mm_EP1.65x1.65mm"})
        );
        assert_eq!(
            bare["changes"]["MPN"],
            json!({"previous": null, "current": "CH347F"})
        );

        let full = call(
            &handler,
            "set_symbol_properties",
            json!({
                "library_path": path, "symbol_name": "FIELDS_FULL",
                "fields": {"MPN": "TPS7A0230PDBVR"}, "keywords": "", "fp_filters": [],
                "description": "200 mA LDO, fixture symbol with every field"
            }),
        )
        .await;
        assert_ne!(full["is_error"], true, "{full}");
        // The unchanged description is not reported.
        assert_eq!(
            full["changes"],
            json!({
                "MPN": {"previous": "TPS7A0233PDBVR", "current": "TPS7A0230PDBVR"},
                "ki_keywords": {"previous": "ldo regulator", "current": null},
                "ki_fp_filters": {"previous": "SOT?23*", "current": null}
            })
        );

        assert_eq!(std::fs::read_to_string(&path).unwrap(), EDITED);
    }

    /// A new custom field lands where a removed `ki_keywords` began.
    #[tokio::test]
    async fn a_field_added_beside_a_removed_one_survives() {
        let handler = handler().await;
        let (_dir, path) = library_with(FIXTURE);

        let body = call(
            &handler,
            "set_symbol_properties",
            json!({
                "library_path": path, "symbol_name": "FIELDS_FULL",
                "keywords": "", "fields": {"Manufacturer": "Texas Instruments"}
            }),
        )
        .await;

        assert_ne!(body["is_error"], true, "{body}");
        let names: Vec<String> = fields_of(&std::fs::read_to_string(&path).unwrap(), "FIELDS_FULL")
            .into_iter()
            .map(|(name, _)| name)
            .collect();
        assert_eq!(
            names,
            [
                "Reference",
                "Value",
                "Footprint",
                "Datasheet",
                "Description",
                "LCSC",
                "MPN",
                "Manufacturer",
                "ki_fp_filters"
            ]
        );
    }

    #[tokio::test]
    async fn an_edit_that_changes_nothing_leaves_the_file_alone() {
        let handler = handler().await;
        let (_dir, path) = library_with(EDITED);
        let before = std::fs::metadata(&path).unwrap().modified().unwrap();

        let body = call(&handler, "set_symbol_properties", bare_edit(&path)).await;

        assert_ne!(body["is_error"], true, "{body}");
        assert_eq!(body["changes"], json!({}));
        assert_eq!(std::fs::read_to_string(&path).unwrap(), EDITED);
        assert_eq!(
            std::fs::metadata(&path).unwrap().modified().unwrap(),
            before
        );
    }

    #[tokio::test]
    async fn create_symbol_writes_every_field_kicad_reads() {
        let handler = handler().await;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("created.kicad_sym");
        let pins = |a: &str, b: &str| {
            json!([
                {"number": "1", "name": a, "type": "power_in"},
                {"number": "2", "name": b, "type": "power_in", "angle": 180}
            ])
        };
        let full = call(
            &handler,
            "create_symbol",
            json!({
                "library_path": path, "name": "FIELDS_FULL", "reference_prefix": "U",
                "value": "TPS7A02", "datasheet": "https://www.ti.com/lit/ds/symlink/tps7a02.pdf",
                "footprint": "Package_TO_SOT_SMD:SOT-23-5",
                "description": "200 mA LDO, fixture symbol with every field",
                "keywords": "ldo regulator", "fp_filters": ["SOT?23*"],
                "fields": {"MPN": "TPS7A0233PDBVR", "LCSC": "C2862740"},
                "pins": pins("IN", "OUT")
            }),
        )
        .await;
        assert_ne!(full["is_error"], true, "{full}");
        let bare = call(
            &handler,
            "create_symbol",
            json!({
                "library_path": path, "name": "FIELDS_BARE", "reference_prefix": "U",
                "value": "CH347F", "pins": pins("VCC", "GND")
            }),
        )
        .await;
        assert_ne!(bare["is_error"], true, "{bare}");

        let created = std::fs::read_to_string(&path).unwrap();
        for symbol in ["FIELDS_FULL", "FIELDS_BARE"] {
            assert_eq!(
                fields_of(&created, symbol),
                fields_of(FIXTURE, symbol),
                "{symbol}"
            );
        }
    }

    #[tokio::test]
    async fn create_symbol_escapes_field_values() {
        let handler = handler().await;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("escaped.kicad_sym");
        let value = "a \"quoted\\path\" value";

        let body = call(
            &handler,
            "create_symbol",
            json!({
                "library_path": path, "name": "ESC", "reference_prefix": "U", "value": value,
                "description": value, "fields": {"Note": value},
                "pins": [{"number": "1", "name": "A", "type": "input"}]
            }),
        )
        .await;

        assert_ne!(body["is_error"], true, "{body}");
        let fields = fields_of(&std::fs::read_to_string(&path).unwrap(), "ESC");
        for name in ["Value", "Description", "Note"] {
            assert!(
                fields.contains(&(name.to_string(), value.to_string())),
                "{name}: {fields:?}"
            );
        }
    }

    #[tokio::test]
    async fn refusals_leave_the_library_unchanged() {
        let handler = handler().await;
        let duplicate =
            FIXTURE.replacen("\t(symbol \"FIELDS_FULL\"", "\t(symbol \"FIELDS_BARE\"", 1);
        for (source, arguments, kind, field) in [
            (
                FIXTURE,
                json!({"fields": {"Footprint": "X:Y"}}),
                "invalid_argument",
                "fields.Footprint",
            ),
            (
                FIXTURE,
                json!({"fields": {"value": "X"}}),
                "invalid_argument",
                "fields.value",
            ),
            (
                FIXTURE,
                json!({"fields": {"ki_keywords": "a"}}),
                "invalid_argument",
                "fields.ki_keywords",
            ),
            (
                FIXTURE,
                json!({"fields": {" MPN": "a"}}),
                "invalid_argument",
                "fields. MPN",
            ),
            (
                FIXTURE,
                json!({"fp_filters": ["SOIC* DIP*"]}),
                "invalid_argument",
                "fp_filters",
            ),
            (
                FIXTURE,
                json!({"fp_filters": [""]}),
                "invalid_argument",
                "fp_filters",
            ),
            (
                FIXTURE,
                json!({"symbol_name": "MISSING", "footprint": "X:Y"}),
                "invalid_argument",
                "symbol_name",
            ),
            (FIXTURE, json!({}), "invalid_argument", "symbol_properties"),
            (
                duplicate.as_str(),
                json!({"footprint": "X:Y"}),
                "ambiguous_target",
                "",
            ),
        ] {
            let (_dir, path) = library_with(source);
            let mut call_arguments = json!({"library_path": path, "symbol_name": "FIELDS_BARE"});
            for (key, value) in arguments.as_object().unwrap() {
                call_arguments[key] = value.clone();
            }
            let body = call(&handler, "set_symbol_properties", call_arguments.clone()).await;
            assert_eq!(body["is_error"], true, "{call_arguments}: {body}");
            assert_eq!(body["error"]["kind"], kind, "{call_arguments}: {body}");
            if !field.is_empty() {
                assert_eq!(body["error"]["field"], field, "{call_arguments}: {body}");
            }
            assert_eq!(
                std::fs::read_to_string(&path).unwrap(),
                source,
                "{call_arguments}"
            );
        }
    }

    #[tokio::test]
    async fn create_symbol_refuses_a_reserved_custom_field_without_writing() {
        let handler = handler().await;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("refused.kicad_sym");

        let body = call(
            &handler,
            "create_symbol",
            json!({
                "library_path": path, "name": "R", "reference_prefix": "U",
                "fields": {"Description": "x"},
                "pins": [{"number": "1", "name": "A", "type": "input"}]
            }),
        )
        .await;

        assert_eq!(body["error"]["field"], "fields.Description", "{body}");
        assert!(!path.exists());
    }

    #[tokio::test]
    async fn a_save_between_read_and_write_is_a_conflict() {
        let handler = handler().await;
        let (_dir, path) = library_with(FIXTURE);
        let newer = FIXTURE.replace("\"CH347F\"", "\"CH347T\"");
        write_hook::set(newer.clone());

        let body = call(&handler, "set_symbol_properties", bare_edit(&path)).await;

        assert_eq!(body["error"]["kind"], "conflict", "{body}");
        assert_eq!(std::fs::read_to_string(&path).unwrap(), newer);
    }

    /// KiCad 7.0.10's stock library, and KiCad 10.0.6's reading of the edit
    /// below; see fiber_optic_kicad7.README.md.
    const KICAD7: &str = include_str!("../../tests/fixtures/fiber_optic_kicad7.kicad_sym");
    const KICAD7_EDITED: &str =
        include_str!("../../tests/fixtures/fiber_optic_kicad7.edited.kicad_sym");
    const KICAD7_RESAVED: &str =
        include_str!("../../tests/fixtures/fiber_optic_kicad7.edited.resaved.kicad_sym");

    #[tokio::test]
    async fn a_kicad7_description_edit_reaches_kicad() {
        let handler = handler().await;
        let (_dir, path) = library_with(KICAD7);

        let body = call(
            &handler,
            "set_symbol_properties",
            json!({
                "library_path": path, "symbol_name": "AFBR-1624Z",
                "description": "Versatile Link transmitter, 650 nm",
                "footprint": "OptoDevice:Broadcom_AFBR-16xxZ_Horizontal",
                "fields": {"MPN": "AFBR-1624Z"}
            }),
        )
        .await;

        assert_ne!(body["is_error"], true, "{body}");
        assert_eq!(
            body["changes"]["Description"],
            json!({"previous": "Versatile Link Fiber Optic Transmitter",
                   "current": "Versatile Link transmitter, 650 nm"})
        );
        assert_eq!(std::fs::read_to_string(&path).unwrap(), KICAD7_EDITED);
        // KiCad's reading of exactly that file.
        let read = fields_of(KICAD7_RESAVED, "AFBR-1624Z");
        for expected in [
            ("Footprint", "OptoDevice:Broadcom_AFBR-16xxZ_Horizontal"),
            ("Description", "Versatile Link transmitter, 650 nm"),
            ("MPN", "AFBR-1624Z"),
        ] {
            assert!(
                read.contains(&(expected.0.to_string(), expected.1.to_string())),
                "{expected:?} not in {read:?}"
            );
        }
    }

    #[tokio::test]
    async fn a_file_that_is_not_a_library_is_not_an_argument_error() {
        let handler = handler().await;
        for (name, source) in [
            ("board.kicad_sym", "(kicad_pcb (version 20241229))\n"),
            ("fields.kicad_pcb", FIXTURE),
        ] {
            let (_dir, path) = library_named(name, source);

            let body = call(
                &handler,
                "set_symbol_properties",
                json!({
                    "library_path": path, "symbol_name": "FIELDS_BARE", "footprint": "X:Y"
                }),
            )
            .await;

            assert_eq!(body["error"]["kind"], "handler_error", "{name}: {body}");
            assert_eq!(std::fs::read_to_string(&path).unwrap(), source, "{name}");
        }
    }

    /// The readback, against the file the bug left: a requested Footprint
    /// that reads back empty.
    #[test]
    fn readback_names_a_field_the_file_does_not_hold() {
        let fields = parse_symbol_fields(&bare_edit(Path::new("x.kicad_sym"))).unwrap();

        let reason = readback_mismatch(FIXTURE, FIXTURE, "FIELDS_BARE", &fields).unwrap();

        assert!(reason.contains("'Footprint' reads Some(\"\")"), "{reason}");
        assert_eq!(
            readback_mismatch(EDITED, EDITED, "FIELDS_BARE", &fields),
            None
        );
    }

    /// Where KiCad's own symbol libraries are installed, if they are.
    pub(in crate::tools) fn installed_symbol_dir() -> Option<std::path::PathBuf> {
        ["KICAD10_SYMBOL_DIR", "KICAD_SYMBOL_DIR"]
            .iter()
            .filter_map(std::env::var_os)
            .map(std::path::PathBuf::from)
            .chain(
                [
                    r"C:\KiCad\10.0\share\kicad\symbols",
                    r"C:\Program Files\KiCad\10.0\share\kicad\symbols",
                    "/usr/share/kicad/symbols",
                    "/usr/local/share/kicad/symbols",
                    "/Applications/KiCad/KiCad.app/Contents/SharedSupport/symbols",
                ]
                .iter()
                .map(std::path::PathBuf::from),
            )
            .find(|dir| dir.is_dir())
    }

    /// The tool's output for one call on FIXTURE, which KiCad 10.0.6 resaves byte
    /// for byte; see symbol_fields_kicad10.renamed.README.md.
    const RENAMED: &str =
        include_str!("../../tests/fixtures/symbol_fields_kicad10.renamed.kicad_sym");

    /// One call on `symbol`, with `edit`'s arguments.
    async fn edit_symbol(
        handler: &McpHandler,
        path: &Path,
        symbol: &str,
        edit: serde_json::Value,
    ) -> serde_json::Value {
        let mut arguments = edit;
        arguments["library_path"] = json!(path);
        arguments["symbol_name"] = json!(symbol);
        call(handler, "set_symbol_properties", arguments).await
    }

    /// `source` without the property block that opens with `opening`, line by
    /// line as KiCad writes it.
    fn without_block(source: &str, opening: &str) -> String {
        let mut out = String::new();
        let mut skipping = false;
        for line in source.split_inclusive('\n') {
            if line.trim_start().starts_with(opening) {
                skipping = true;
            }
            if !skipping {
                out.push_str(line);
            } else if line.trim_end_matches(['\r', '\n']) == "\t\t)" {
                skipping = false;
            }
        }
        out
    }

    #[tokio::test]
    async fn a_value_a_rename_and_a_removal_write_what_kicad_resaves() {
        let handler = handler().await;
        let (_dir, path) = library_with(FIXTURE);
        let body = edit_symbol(
            &handler,
            &path,
            "FIELDS_FULL",
            json!({"value": "TPS7A0230", "rename_fields": {"LCSC": "LCSC Part"}, "remove_fields": ["MPN"]}),
        )
        .await;
        assert_ne!(body["is_error"], true, "{body}");
        assert_eq!(std::fs::read_to_string(&path).unwrap(), RENAMED);
    }

    /// A re-run after the rename landed names the field that is there now,
    /// once, as the rename it was.
    #[tokio::test]
    async fn a_rename_run_twice_names_the_field_that_is_there() {
        let handler = handler().await;
        for (old, new) in [("LCSC", "LCSC Part"), ("MPN", "mpn")] {
            let (_dir, path) = library_with(FIXTURE);
            let edit = json!({"rename_fields": {old: new}});
            let first = edit_symbol(&handler, &path, "FIELDS_FULL", edit.clone()).await;
            assert_ne!(first["is_error"], true, "{first}");
            let after = std::fs::read_to_string(&path).unwrap();
            let again = edit_symbol(&handler, &path, "FIELDS_FULL", edit).await;
            assert_eq!(
                again["error"]["field"],
                format!("rename_fields.{old}"),
                "{again}"
            );
            let reason = again["error"]["reason"].as_str().unwrap_or_default();
            assert_eq!(reason.matches(&format!("'{new}'")).count(), 1, "{reason}");
            assert!(
                reason.contains(&format!("has no field '{old}'; it has a field '{new}'")),
                "{reason}"
            );
            assert_eq!(std::fs::read_to_string(&path).unwrap(), after);
        }
    }

    #[tokio::test]
    async fn a_rename_changes_only_the_name_token() {
        let handler = handler().await;
        let (_dir, path) = library_with(FIXTURE);
        assert_eq!(FIXTURE.matches("(property \"LCSC\"").count(), 1);

        let body = edit_symbol(
            &handler,
            &path,
            "FIELDS_FULL",
            json!({"rename_fields": {"LCSC": "LCSC Part"}}),
        )
        .await;
        assert_ne!(body["is_error"], true, "{body}");
        assert_eq!(
            body["changes"],
            json!({
                "LCSC": {"previous": "C2862740", "current": null},
                "LCSC Part": {"previous": null, "current": "C2862740"}
            })
        );
        assert!(body.get("removed_fields").is_none() && body.get("absent_fields").is_none());
        let renamed = FIXTURE.replacen("(property \"LCSC\"", "(property \"LCSC Part\"", 1);
        assert_eq!(std::fs::read_to_string(&path).unwrap(), renamed);

        // KiCad keeps custom field names case-sensitively, so a case-only
        // rename is a rename.
        let body = edit_symbol(
            &handler,
            &path,
            "FIELDS_FULL",
            json!({"rename_fields": {"MPN": "mpn"}}),
        )
        .await;
        assert_ne!(body["is_error"], true, "{body}");
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            renamed.replacen("(property \"MPN\"", "(property \"mpn\"", 1)
        );
    }

    #[tokio::test]
    async fn a_value_edit_replaces_only_the_value_token() {
        let handler = handler().await;
        let (_dir, path) = library_with(FIXTURE);
        assert_eq!(FIXTURE.matches("\"CH347F\"").count(), 1);

        let body = edit_symbol(&handler, &path, "FIELDS_BARE", json!({"value": "CH347T"})).await;
        assert_eq!(
            body["changes"],
            json!({"Value": {"previous": "CH347F", "current": "CH347T"}}),
            "{body}"
        );
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            FIXTURE.replacen("\"CH347F\"", "\"CH347T\"", 1)
        );
    }

    /// KiCad's stock `Regulator_Current` library, in which `HV101K5-G` extends
    /// `HV100K5-G` and carries its own Value. Read from the installed library
    /// rather than committed, because the library is CC BY-SA, as the footprint
    /// and demo tests do. Skips without an installed KiCad, or when the installed
    /// library no longer gives `HV101K5-G` its own Value. With KiCad 10.0.6's
    /// copy, `kicad-cli sym upgrade --force` resaves this call's output byte for
    /// byte.
    #[tokio::test]
    async fn a_derived_symbols_value_changes_only_its_own_block() {
        let Some(dir) = installed_symbol_dir() else {
            eprintln!("SKIP: no installed KiCad symbol library");
            return;
        };
        let library = dir.join("Regulator_Current.kicad_sym");
        let Ok(derived) = std::fs::read_to_string(&library) else {
            eprintln!("SKIP: {} is not installed", library.display());
            return;
        };
        let own = "(property \"Value\" \"HV101K5-G\"";
        if derived.matches(own).count() != 1 || !derived.contains("(extends \"HV100K5-G\")") {
            eprintln!("SKIP: {} no longer derives HV101K5-G so", library.display());
            return;
        }
        let handler = handler().await;
        let (_dir, path) = library_with(&derived);

        let body = edit_symbol(
            &handler,
            &path,
            "HV101K5-G",
            json!({"value": "HV101K5-G/TR"}),
        )
        .await;
        assert_eq!(
            body["changes"],
            json!({"Value": {"previous": "HV101K5-G", "current": "HV101K5-G/TR"}}),
            "{body}"
        );
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            derived.replacen(own, "(property \"Value\" \"HV101K5-G/TR\"", 1)
        );
    }

    #[tokio::test]
    async fn a_removed_field_leaves_the_rest_as_written() {
        let handler = handler().await;
        let (_dir, path) = library_with(FIXTURE);
        let body = edit_symbol(
            &handler,
            &path,
            "FIELDS_FULL",
            json!({"remove_fields": ["MPN"]}),
        )
        .await;
        assert_eq!(
            body["changes"],
            json!({"MPN": {"previous": "TPS7A0233PDBVR", "current": null}}),
            "{body}"
        );
        assert_eq!(body["removed_fields"], json!(["MPN"]));
        assert_eq!(body["absent_fields"], json!([]));
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            without_block(FIXTURE, "(property \"MPN\"")
        );
    }

    #[tokio::test]
    async fn a_name_listed_twice_is_removed_once() {
        let handler = handler().await;
        let (_dir, path) = library_with(FIXTURE);
        let body = edit_symbol(
            &handler,
            &path,
            "FIELDS_FULL",
            json!({"remove_fields": ["MPN", "MPN"]}),
        )
        .await;
        assert_eq!(body["removed_fields"], json!(["MPN"]), "{body}");
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            without_block(FIXTURE, "(property \"MPN\"")
        );
    }

    #[tokio::test]
    async fn a_case_variant_is_another_field() {
        let handler = handler().await;
        let (_dir, path) = library_with(FIXTURE);

        let body = edit_symbol(
            &handler,
            &path,
            "FIELDS_FULL",
            json!({"remove_fields": ["mpn"]}),
        )
        .await;
        assert_eq!(body["absent_fields"], json!(["mpn"]), "{body}");
        let body = edit_symbol(
            &handler,
            &path,
            "FIELDS_FULL",
            json!({"rename_fields": {"lcsc": "X"}}),
        )
        .await;
        assert_eq!(body["error"]["field"], "rename_fields.lcsc", "{body}");
        assert!(
            body.to_string()
                .contains("it has 'LCSC', which KiCad treats as a different field"),
            "{body}"
        );
        assert_eq!(std::fs::read_to_string(&path).unwrap(), FIXTURE);

        let body = edit_symbol(
            &handler,
            &path,
            "FIELDS_FULL",
            json!({"rename_fields": {"LCSC": "mpn"}}),
        )
        .await;
        assert_ne!(body["is_error"], true, "{body}");
        let names: Vec<String> = fields_of(&std::fs::read_to_string(&path).unwrap(), "FIELDS_FULL")
            .into_iter()
            .map(|(name, _)| name)
            .collect();
        assert!(
            names.contains(&"mpn".to_string()) && names.contains(&"MPN".to_string()),
            "{names:?}"
        );
    }

    #[tokio::test]
    async fn a_removed_name_is_matched_whole() {
        let handler = handler().await;
        let (_dir, path) = library_with(FIXTURE);
        let body = edit_symbol(
            &handler,
            &path,
            "FIELDS_FULL",
            json!({"remove_fields": ["MP"]}),
        )
        .await;
        assert_eq!(body["absent_fields"], json!(["MP"]), "{body}");
        assert_eq!(std::fs::read_to_string(&path).unwrap(), FIXTURE);
    }

    #[tokio::test]
    async fn removing_absent_fields_reports_them_and_writes_nothing() {
        let handler = handler().await;
        let (_dir, path) = library_with(FIXTURE);
        let before = std::fs::metadata(&path).unwrap().modified().unwrap();
        let body = edit_symbol(
            &handler,
            &path,
            "FIELDS_FULL",
            json!({"remove_fields": ["Tolerance", "Tolerance"]}),
        )
        .await;
        assert_eq!(body["changes"], json!({}), "{body}");
        assert_eq!(body["removed_fields"], json!([]));
        assert_eq!(body["absent_fields"], json!(["Tolerance"]));
        assert_eq!(std::fs::read_to_string(&path).unwrap(), FIXTURE);
        assert_eq!(
            std::fs::metadata(&path).unwrap().modified().unwrap(),
            before
        );
    }

    #[tokio::test]
    async fn an_identity_rename_changes_nothing() {
        let handler = handler().await;
        let (_dir, path) = library_with(FIXTURE);
        let before = std::fs::metadata(&path).unwrap().modified().unwrap();
        let body = edit_symbol(
            &handler,
            &path,
            "FIELDS_FULL",
            json!({"rename_fields": {"LCSC": "LCSC"}}),
        )
        .await;
        assert_ne!(body["is_error"], true, "{body}");
        assert_eq!(body["changes"], json!({}));
        assert!(body.get("removed_fields").is_none() && body.get("absent_fields").is_none());
        assert_eq!(std::fs::read_to_string(&path).unwrap(), FIXTURE);
        assert_eq!(
            std::fs::metadata(&path).unwrap().modified().unwrap(),
            before
        );
    }

    #[tokio::test]
    async fn a_field_added_where_one_is_removed_survives() {
        let handler = handler().await;
        let (_dir, path) = library_with(FIXTURE);
        // MPN is the last custom field, so it is the new field's anchor.
        let body = edit_symbol(
            &handler,
            &path,
            "FIELDS_FULL",
            json!({"remove_fields": ["MPN"], "fields": {"Manufacturer": "Texas Instruments"}}),
        )
        .await;
        assert_ne!(body["is_error"], true, "{body}");
        let names: Vec<String> = fields_of(&std::fs::read_to_string(&path).unwrap(), "FIELDS_FULL")
            .into_iter()
            .map(|(name, _)| name)
            .collect();
        assert_eq!(
            names,
            [
                "Reference",
                "Value",
                "Footprint",
                "Datasheet",
                "Description",
                "LCSC",
                "Manufacturer",
                "ki_keywords",
                "ki_fp_filters"
            ]
        );
    }

    #[tokio::test]
    async fn a_field_added_then_removed_gives_back_the_kicad_file() {
        let handler = handler().await;
        let (_dir, path) = library_with(FIXTURE);
        for edit in [
            json!({"fields": {"Note": "x"}}),
            json!({"remove_fields": ["Note"]}),
        ] {
            let body = edit_symbol(&handler, &path, "FIELDS_BARE", edit).await;
            assert_ne!(body["is_error"], true, "{body}");
        }
        assert_eq!(std::fs::read_to_string(&path).unwrap(), FIXTURE);
    }

    #[tokio::test]
    async fn a_crlf_library_stays_crlf() {
        let handler = handler().await;
        let edit = json!({"value": "TPS7A0230", "rename_fields": {"LCSC": "LCSC Part"}, "remove_fields": ["MPN"]});
        let (_lf_dir, lf) = library_with(FIXTURE);
        let (_crlf_dir, crlf) = library_with(&FIXTURE.replace('\n', "\r\n"));
        for path in [&lf, &crlf] {
            let body = edit_symbol(&handler, path, "FIELDS_FULL", edit.clone()).await;
            assert_ne!(body["is_error"], true, "{body}");
        }
        let lf = std::fs::read_to_string(&lf).unwrap();
        let crlf = std::fs::read_to_string(&crlf).unwrap();
        assert_eq!(crlf, lf.replace('\n', "\r\n"));
        assert!(!crlf.replace("\r\n", "").contains('\n'));
    }

    #[tokio::test]
    async fn a_private_field_is_read_after_its_keyword() {
        let handler = handler().await;
        let private = FIXTURE.replacen("(property \"LCSC\"", "(property private \"LCSC\"", 1);
        let (_dir, path) = library_with(&private);

        let body = edit_symbol(
            &handler,
            &path,
            "FIELDS_FULL",
            json!({"rename_fields": {"private": "X"}}),
        )
        .await;
        assert!(
            body.to_string().contains("has no field 'private'"),
            "{body}"
        );
        let body = edit_symbol(
            &handler,
            &path,
            "FIELDS_FULL",
            json!({"rename_fields": {"LCSC": "Supplier"}}),
        )
        .await;
        assert_ne!(body["is_error"], true, "{body}");
        let renamed = private.replacen(
            "(property private \"LCSC\"",
            "(property private \"Supplier\"",
            1,
        );
        assert_eq!(std::fs::read_to_string(&path).unwrap(), renamed);

        let body = edit_symbol(
            &handler,
            &path,
            "FIELDS_FULL",
            json!({"remove_fields": ["Supplier"]}),
        )
        .await;
        assert_eq!(body["removed_fields"], json!(["Supplier"]), "{body}");
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            without_block(&renamed, "(property private \"Supplier\"")
        );
    }

    #[tokio::test]
    async fn value_on_a_symbol_without_a_value_field_is_refused() {
        let handler = handler().await;
        let folded = FIXTURE.replacen(
            "(property \"Value\" \"CH347F\"",
            "(property \"value\" \"CH347F\"",
            1,
        );
        let (_dir, path) = library_with(&folded);
        let body = edit_symbol(&handler, &path, "FIELDS_BARE", json!({"value": "X"})).await;
        assert_eq!(body["error"]["kind"], "handler_error", "{body}");
        assert!(
            body.to_string()
                .contains("it has 'value', which KiCad reads as Value"),
            "{body}"
        );
        assert_eq!(std::fs::read_to_string(&path).unwrap(), folded);
    }

    #[tokio::test]
    async fn rename_and_remove_refusals_leave_the_library_unchanged() {
        let handler = handler().await;
        for (arguments, field, reason) in [
            (
                json!({"rename_fields": {"Reference": "Ref"}}),
                "rename_fields.Reference",
                "whose names cannot change",
            ),
            (
                json!({"rename_fields": {"value": "X"}}),
                "rename_fields.value",
                "whose names cannot change",
            ),
            (
                json!({"rename_fields": {"ki_keywords": "Keywords"}}),
                "rename_fields.ki_keywords",
                "is a ki_* name",
            ),
            (
                json!({"rename_fields": {"MPN": "datasheet"}}),
                "rename_fields.MPN",
                "cannot take that name",
            ),
            (
                json!({"rename_fields": {"MPN": "KI_DESCRIPTION"}}),
                "rename_fields.MPN",
                "is a ki_* name",
            ),
            (
                json!({"rename_fields": {"MPN": "${MPN}"}}),
                "rename_fields.MPN",
                "generated field",
            ),
            (
                json!({"rename_fields": {"MPN": " Part"}}),
                "rename_fields.MPN",
                "without surrounding whitespace",
            ),
            (
                json!({"rename_fields": {"MPN": ""}}),
                "rename_fields.MPN",
                "without surrounding whitespace",
            ),
            (
                json!({"rename_fields": {"LCSC": "X", "MPN": "X"}}),
                "rename_fields.MPN",
                "is also the new name of 'LCSC'",
            ),
            (
                json!({"rename_fields": {"LCSC": "MPN", "MPN": "Part"}}),
                "rename_fields.LCSC",
                "chain or swap",
            ),
            (
                json!({"rename_fields": {"LCSC": "MPN", "MPN": "LCSC"}}),
                "rename_fields.LCSC",
                "chain or swap",
            ),
            (
                json!({"rename_fields": {"LCSC": "X"}, "fields": {"X": "1"}}),
                "rename_fields.LCSC",
                "also named in fields",
            ),
            (
                json!({"rename_fields": {"LCSC": "X"}, "fields": {"LCSC": "1"}}),
                "rename_fields.LCSC",
                "also named in fields",
            ),
            (
                json!({"rename_fields": {"Missing": "X"}}),
                "rename_fields.Missing",
                "has no field 'Missing'",
            ),
            (
                json!({"rename_fields": {"LCSC": "MPN"}}),
                "rename_fields.LCSC",
                "already has a field 'MPN'",
            ),
            (
                json!({"remove_fields": ["Value"]}),
                "remove_fields",
                "cannot be removed",
            ),
            (
                json!({"remove_fields": ["description"]}),
                "remove_fields",
                "cannot be removed",
            ),
            (
                json!({"remove_fields": ["ki_fp_filters"]}),
                "remove_fields",
                "is a ki_* name",
            ),
            (
                json!({"remove_fields": [""]}),
                "remove_fields",
                "non-empty field name",
            ),
            (
                json!({"remove_fields": ["MPN"], "fields": {"MPN": "x"}}),
                "remove_fields",
                "also named in fields",
            ),
            (
                json!({"remove_fields": ["LCSC"], "rename_fields": {"LCSC": "X"}}),
                "remove_fields",
                "also named in rename_fields",
            ),
            (
                json!({"remove_fields": ["X"], "rename_fields": {"LCSC": "X"}}),
                "remove_fields",
                "also named in rename_fields",
            ),
            (
                json!({"rename_fields": null}),
                "rename_fields",
                "null is not of type \\\"object\\\"",
            ),
            (
                json!({"remove_fields": null}),
                "remove_fields",
                "null is not of type \\\"array\\\"",
            ),
            (
                json!({"rename_fields": {}, "remove_fields": []}),
                "symbol_properties",
                "supply at least one of value, footprint, description, keywords, fp_filters, fields, rename_fields or remove_fields",
            ),
        ] {
            let (_dir, path) = library_with(FIXTURE);
            let body = edit_symbol(&handler, &path, "FIELDS_FULL", arguments.clone()).await;
            assert_eq!(body["is_error"], true, "{arguments}: {body}");
            assert_eq!(
                body["error"]["kind"], "invalid_argument",
                "{arguments}: {body}"
            );
            assert_eq!(body["error"]["field"], field, "{arguments}: {body}");
            assert!(body.to_string().contains(reason), "{arguments}: {body}");
            assert_eq!(
                std::fs::read_to_string(&path).unwrap(),
                FIXTURE,
                "{arguments}"
            );
        }
    }

    /// The handler's own shape checks, beneath the schema the dispatch
    /// enforces first.
    #[tokio::test]
    async fn rename_and_remove_shape_refusals_without_dispatch() {
        let context = ToolContext::new(
            ServerConfig::default(),
            std::sync::Arc::new(crate::router::ToolRouter::new()),
        );
        for (arguments, field, reason) in [
            (
                json!({"rename_fields": "MPN"}),
                "rename_fields",
                "must be an object",
            ),
            (
                json!({"rename_fields": null}),
                "rename_fields",
                "must be an object",
            ),
            (
                json!({"rename_fields": {"MPN": 7}}),
                "rename_fields.MPN",
                "must be a string",
            ),
            (
                json!({"remove_fields": "MPN"}),
                "remove_fields",
                "expected an array of field names",
            ),
            (
                json!({"remove_fields": null}),
                "remove_fields",
                "expected an array of field names",
            ),
            (
                json!({"remove_fields": [7]}),
                "remove_fields",
                "non-empty field name",
            ),
            (json!({"value": 7}), "value", "must be a string"),
        ] {
            let (_dir, path) = library_with(FIXTURE);
            let mut call_arguments = arguments.clone();
            call_arguments["library_path"] = json!(path);
            call_arguments["symbol_name"] = json!("FIELDS_FULL");
            let result = handle_set_symbol_properties(&call_arguments, &context)
                .await
                .unwrap();
            assert!(result.is_error, "{arguments}");
            assert_eq!(
                crate::mcp::error::extract_error_kind(&result).as_deref(),
                Some("invalid_argument"),
                "{arguments}"
            );
            let crate::mcp::protocol::ToolContent::Text { text } = &result.content[0] else {
                panic!("expected text");
            };
            assert!(text.contains(reason), "{arguments}: {text}");
            let body: serde_json::Value = serde_json::from_str(text).unwrap();
            assert_eq!(body["error"]["field"], field, "{arguments}: {text}");
            assert_eq!(
                std::fs::read_to_string(&path).unwrap(),
                FIXTURE,
                "{arguments}"
            );
        }
    }

    #[tokio::test]
    async fn fields_names_the_value_argument() {
        let handler = handler().await;
        let (_dir, path) = library_with(FIXTURE);
        for (name, reason) in [
            ("Value", "use the value, footprint or description argument"),
            ("Datasheet", "Reference and Datasheet are mandatory fields"),
        ] {
            let body = edit_symbol(
                &handler,
                &path,
                "FIELDS_FULL",
                json!({"fields": {name: "x"}}),
            )
            .await;
            assert!(body.to_string().contains(reason), "{name}: {body}");
            let dir = tempfile::tempdir().unwrap();
            let body = call(
                &handler,
                "create_symbol",
                json!({
                    "library_path": dir.path().join("refused.kicad_sym"), "name": "R",
                    "reference_prefix": "U", "fields": {name: "x"},
                    "pins": [{"number": "1", "name": "A", "type": "input"}]
                }),
            )
            .await;
            assert!(
                body.to_string().contains(reason),
                "create_symbol {name}: {body}"
            );
        }
    }

    #[test]
    fn readback_names_a_removal_the_file_still_holds() {
        let fields = SymbolFields {
            removals: vec!["LCSC".into()],
            ..SymbolFields::default()
        };
        let reason = readback_mismatch(FIXTURE, FIXTURE, "FIELDS_FULL", &fields).unwrap();
        assert!(
            reason.contains("still present after its removal"),
            "{reason}"
        );
        let removed = without_block(FIXTURE, "(property \"LCSC\"");
        assert_eq!(
            readback_mismatch(&removed, FIXTURE, "FIELDS_FULL", &fields),
            None
        );
    }

    #[test]
    fn readback_names_a_rename_that_did_not_keep_its_layout() {
        let fields = SymbolFields {
            renames: vec![("LCSC".into(), "LCSC Part".into())],
            ..SymbolFields::default()
        };
        let reason = readback_mismatch(FIXTURE, FIXTURE, "FIELDS_FULL", &fields).unwrap();
        assert!(
            reason.contains("still present after its rename"),
            "{reason}"
        );

        let renamed = FIXTURE.replacen("(property \"LCSC\"", "(property \"LCSC Part\"", 1);
        assert_eq!(
            readback_mismatch(&renamed, FIXTURE, "FIELDS_FULL", &fields),
            None
        );

        // The renamed block without its (hide yes): the name is right, the
        // field is not the one renamed.
        let at = renamed.find("(property \"LCSC Part\"").unwrap();
        let hide = at + renamed[at..].find("\t\t\t(hide yes)\n").unwrap();
        let unhidden = format!(
            "{}{}",
            &renamed[..hide],
            &renamed[hide + "\t\t\t(hide yes)\n".len()..]
        );
        let reason = readback_mismatch(&unhidden, FIXTURE, "FIELDS_FULL", &fields).unwrap();
        assert!(reason.contains("is not 'LCSC' renamed"), "{reason}");
    }
}
