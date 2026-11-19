//! `rename_footprint`: rename a footprint in its `.pretty` library, as KiCad's
//! footprint editor does (`FOOTPRINT_EDITOR_CONTROL::RenameFootprint`,
//! `pcbnew/tools/footprint_editor_control.cpp`, 10.0.8).

use crate::mcp::protocol::CallToolResult;
use crate::tool;
use crate::tools::footprint_metadata::quote_string;
use crate::tools::{invalid_arg, ToolContext, ToolDef};
use copperpour_sexp::writer::find_direct_child_blocks;
use copperpour_sexp::{parse_sexp, SexpError, SexpNode};
use serde_json::json;
use std::path::{Path, PathBuf};

const OPERATION: &str = "rename_footprint";

/// The characters KiCad refuses in a footprint name
/// (`FOOTPRINT::StringLibNameInvalidChars`, `pcbnew/footprint.cpp`).
const INVALID_CHARACTERS: &str = "%$<>\t\n\r\"\\/:";

/// Characters a Windows file name cannot hold, beyond KiCad's own list.
const WINDOWS_INVALID_CHARACTERS: &str = "*?|";

/// Names Windows reserves for devices, whatever the extension.
const WINDOWS_RESERVED_NAMES: [&str; 22] = [
    "CON", "PRN", "AUX", "NUL", "COM1", "COM2", "COM3", "COM4", "COM5", "COM6", "COM7", "COM8",
    "COM9", "LPT1", "LPT2", "LPT3", "LPT4", "LPT5", "LPT6", "LPT7", "LPT8", "LPT9",
];

pub(crate) fn tool() -> ToolDef {
    tool!(
        "rename_footprint",
        "Rename a footprint in its .pretty library, as KiCad's footprint editor does: the file \
         becomes NEW_NAME.kicad_mod with the new name inside, and the Value changes too when \
         it equals the old name. Refuses a name already in the library, including one that \
         differs only in case, since both would be one file on a case-insensitive filesystem; \
         a rename that changes only the case of the same footprint is allowed. Only this \
         library changes: symbols whose Footprint field names the footprint, and footprints \
         placed on boards, keep the old library id, as in KiCad.",
        json!({
            "type": "object",
            "properties": {
                "library_path": {
                    "type": "string",
                    "description": "Path to the .pretty footprint library directory"
                },
                "footprint_name": {
                    "type": "string",
                    "description": "The footprint's current name, its file name without .kicad_mod"
                },
                "new_name": {
                    "type": "string",
                    "description": "The new name. KiCad refuses % $ < > \" \\ / : and tabs or line breaks in a footprint name; on Windows * ? | and device names such as CON or NUL cannot be file names either."
                }
            },
            "required": ["library_path", "footprint_name", "new_name"],
            "additionalProperties": false
        }),
        |args, ctx| async move { handle_rename_footprint(args, ctx).await }
    )
}

async fn handle_rename_footprint(
    args: &serde_json::Value,
    _ctx: &ToolContext,
) -> anyhow::Result<CallToolResult> {
    let library = super::get_path(args, "library_path")?;
    let old_name = match super::require_str(args, "footprint_name") {
        Ok(name) => name,
        Err(error) => return Ok(error),
    };
    let new_name = match super::require_str(args, "new_name") {
        Ok(name) => name,
        Err(error) => return Ok(error),
    };
    if let Err(reason) = name_reason(new_name) {
        return Ok(invalid_arg("new_name", &reason));
    }
    if !is_pretty(&library) {
        return Ok(unusable(&library, "it is not a .pretty directory"));
    }
    if !library.is_dir() {
        return Ok(CallToolResult::error_kind(
            crate::mcp::error::ToolErrorKind::file_not_found(&library),
            format!("Footprint library not found: {}", library.display()),
        ));
    }
    if new_name == old_name {
        return Ok(invalid_arg(
            "new_name",
            "it is already the footprint's name, so there is nothing to rename",
        ));
    }

    let names = footprint_names(&library)?;
    let old_path = library.join(format!("{old_name}.kicad_mod"));
    let new_path = library.join(format!("{new_name}.kicad_mod"));
    let case_only = same_ignoring_case(old_name, new_name);
    if case_only
        && !names.iter().any(|name| name == old_name)
        && names.iter().any(|name| name == new_name)
    {
        // A case-only rename interrupted after renaming the file: it still
        // holds the old name and the original text, so finish it from there.
        let source = copperpour_sexp::read_consistent(&new_path)?;
        if footprint_name(&source).as_deref() == Some(old_name) {
            let renamed = match renamed_source(&source, old_name, new_name) {
                Ok(renamed) => renamed,
                Err(reason) => return Ok(unusable(&new_path, &reason)),
            };
            return rewrite_case_only(&library, None, &new_path, &source, old_name, &renamed, true);
        }
    }
    if !names.iter().any(|name| name == old_name) {
        let hint = if names.iter().any(|name| name == new_name) {
            format!(" A footprint '{new_name}' is in the library: it may be renamed already.")
        } else {
            match names.iter().find(|name| same_ignoring_case(name, old_name)) {
                Some(found) => format!(" '{found}' differs only in case."),
                None => String::new(),
            }
        };
        return Ok(invalid_arg(
            "footprint_name",
            &format!("no footprint '{old_name}' in the library.{hint}"),
        ));
    }

    let source = copperpour_sexp::read_consistent(&old_path)?;
    let renamed = match renamed_source(&source, old_name, new_name) {
        Ok(renamed) => renamed,
        Err(reason) => return Ok(unusable(&old_path, &reason)),
    };

    if let Some(taken) = names
        .iter()
        .find(|name| *name != old_name && same_ignoring_case(name, new_name))
    {
        // A rename interrupted after writing the new file: finish it.
        let interrupted = taken == new_name
            && copperpour_sexp::read_consistent(&new_path).is_ok_and(|text| text == renamed.text);
        if !interrupted {
            let reason = if taken == new_name {
                format!("a footprint '{new_name}' is already in the library; rename_footprint does not overwrite one")
            } else {
                format!(
                    "the library has a footprint '{taken}', which differs only in case; both \
                     would be one file on a case-insensitive filesystem"
                )
            };
            return Ok(invalid_arg("new_name", &reason));
        }
        return finish_interrupted(&library, &old_path, &new_path, &source, old_name, &renamed);
    }

    if case_only {
        // One file on a case-insensitive filesystem. Rename it first, then
        // rewrite it: if the rewrite does not happen, the file keeps its
        // original text, and the next call finishes from that with the same
        // answer, the Value included. A file that already holds the new name
        // was rewritten before its rename by an earlier version of this tool;
        // its old Value is gone, so that answer cannot report one.
        let finished_earlier = footprint_name(&source).as_deref() == Some(new_name);
        std::fs::rename(&old_path, &new_path)?;
        return rewrite_case_only(
            &library,
            Some(&old_path),
            &new_path,
            &source,
            old_name,
            &renamed,
            finished_earlier,
        );
    }

    match copperpour_sexp::write_new_atomic_verified(&new_path, &renamed.text) {
        Ok(()) => {}
        Err(SexpError::Io(error)) if error.kind() == std::io::ErrorKind::AlreadyExists => {
            return Ok(super::revision_conflict_result(&new_path, OPERATION));
        }
        Err(SexpError::ReadbackMismatch { path }) => {
            return Ok(super::readback_mismatch_result(&path, OPERATION))
        }
        Err(error) => return Err(error.into()),
    }
    remove_old(&library, &old_path, &new_path, &source, old_name, &renamed)
}

/// Delete the old file now that the new one is written. If the old file
/// changed meanwhile, the new one is taken back, so nothing is renamed.
fn remove_old(
    library: &Path,
    old_path: &Path,
    new_path: &Path,
    source: &str,
    old_name: &str,
    renamed: &Renamed,
) -> anyhow::Result<CallToolResult> {
    let new_name = &renamed.name;
    match copperpour_sexp::remove_if_unchanged(old_path, source) {
        Ok(()) => Ok(answer(library, old_name, new_name, renamed)),
        Err(SexpError::Conflict { path }) => {
            match copperpour_sexp::remove_if_unchanged(new_path, &renamed.text) {
                Ok(()) => Ok(super::revision_conflict_result(&path, OPERATION)),
                Err(error) => Ok(super::mutation_outcome_uncertain(
                    new_path,
                    OPERATION,
                    format!(
                        "'{old_name}' changed after rename_footprint read it, so the new file \
                         '{new_name}.kicad_mod' it had written was to be removed again, but \
                         that failed ({error}); both files are in the library"
                    ),
                )),
            }
        }
        Err(error) => Ok(super::mutation_outcome_uncertain(
            old_path,
            OPERATION,
            format!(
                "'{new_name}.kicad_mod' is written, but '{old_name}.kicad_mod' could not be \
                 removed ({error}); call rename_footprint again to finish"
            ),
        )),
    }
}

/// The new file holds exactly what this rename writes and the old file is
/// still there: an earlier call stopped in between. Remove the old file.
fn finish_interrupted(
    library: &Path,
    old_path: &Path,
    new_path: &Path,
    source: &str,
    old_name: &str,
    renamed: &Renamed,
) -> anyhow::Result<CallToolResult> {
    let result = remove_old(library, old_path, new_path, source, old_name, renamed)?;
    if result.is_error {
        return Ok(result);
    }
    let mut body = answer_body(library, old_name, &renamed.name, renamed);
    body["finished_earlier_rename"] = json!(true);
    Ok(CallToolResult::json(&body))
}

/// Rewrite a case-only rename's file, already at `new_path`, with the new
/// name. `rename_back` is the file's old path when this call renamed it: if
/// the footprint was saved in between, the file gets its old name back, so
/// nothing is renamed.
fn rewrite_case_only(
    library: &Path,
    rename_back: Option<&Path>,
    new_path: &Path,
    source: &str,
    old_name: &str,
    renamed: &Renamed,
    finished_earlier: bool,
) -> anyhow::Result<CallToolResult> {
    let new_name = &renamed.name;
    match copperpour_sexp::write_atomic_if_unchanged(new_path, source, &renamed.text) {
        Ok(()) => {}
        Err(SexpError::Conflict { path }) => {
            let Some(old_path) = rename_back else {
                return Ok(super::revision_conflict_result(&path, OPERATION));
            };
            return Ok(match std::fs::rename(new_path, old_path) {
                Ok(()) => super::revision_conflict_result(old_path, OPERATION),
                Err(error) => super::mutation_outcome_uncertain(
                    new_path,
                    OPERATION,
                    format!(
                        "'{old_name}' changed after rename_footprint read it, so its file, \
                         already renamed to '{new_name}.kicad_mod', was to get its old name \
                         back, but that failed ({error}); it holds what was saved, under the \
                         new file name"
                    ),
                ),
            });
        }
        Err(SexpError::ReadbackMismatch { path }) => {
            return Ok(super::readback_mismatch_result(&path, OPERATION))
        }
        Err(error) => {
            return Ok(super::mutation_outcome_uncertain(
                new_path,
                OPERATION,
                format!(
                    "the file is renamed to '{new_name}.kicad_mod', but it still holds the name \
                     '{old_name}' ({error}); call rename_footprint again to finish"
                ),
            ))
        }
    }
    let mut body = answer_body(library, old_name, new_name, renamed);
    if finished_earlier {
        body["finished_earlier_rename"] = json!(true);
    }
    Ok(CallToolResult::json(&body))
}

/// The name in a footprint file's header, if it reads as one.
fn footprint_name(source: &str) -> Option<String> {
    let root = parse_sexp(source).ok()?;
    if root.head() != Some("footprint") {
        return None;
    }
    root.get(1)?.as_str().map(str::to_string)
}

struct Renamed {
    name: String,
    text: String,
    /// The Value before and after, when the rename changed it.
    value: Option<(String, String)>,
}

/// The footprint file renamed as KiCad writes it: the name in its header, and
/// the Value when it equals the old name. Or why the file cannot be renamed.
fn renamed_source(source: &str, old_name: &str, new_name: &str) -> Result<Renamed, String> {
    let root = parse_sexp(source).map_err(|error| format!("it does not parse: {error}"))?;
    if root.head() != Some("footprint") {
        return Err(super::footprint_root_reason(root.head()));
    }
    let (_, head_end) =
        super::library::skip_sexp_token(source, 1).ok_or("it has no footprint header")?;
    let (name_start, name_end) =
        super::library::skip_sexp_token(source, head_end).ok_or("it has no footprint name")?;
    if !matches!(root.get(1), Some(SexpNode::Str(_) | SexpNode::Atom(_))) {
        return Err("it has no footprint name".to_string());
    }
    let mut edits = vec![copperpour_sexp::SexpEdit::replace(
        name_start,
        name_end,
        quote_string(new_name),
    )];
    let mut value = None;
    if let Some((span, text)) = value_span(source) {
        if text == old_name {
            edits.push(copperpour_sexp::SexpEdit::replace(
                span.0,
                span.1,
                quote_string(new_name),
            ));
            value = Some((text, new_name.to_string()));
        }
    }
    Ok(Renamed {
        name: new_name.to_string(),
        text: copperpour_sexp::apply_edits(source.to_string(), edits),
        value,
    })
}

/// The span and text of the footprint's Value: `(property "Value" "…")`, or
/// `(fp_text value "…")` in an older file.
fn value_span(source: &str) -> Option<((usize, usize), String)> {
    for (start, end) in find_direct_child_blocks(source, "footprint") {
        let Ok(node) = parse_sexp(&source[start..end]) else {
            continue;
        };
        let is_value = match node.head() {
            Some("property") => node.get(1).and_then(SexpNode::as_str) == Some("Value"),
            Some("fp_text") => node.get(1).and_then(SexpNode::as_str) == Some("value"),
            _ => false,
        };
        if !is_value {
            continue;
        }
        let text = node.get(2).and_then(SexpNode::as_str)?.to_string();
        let (_, head_end) = super::library::skip_sexp_token(source, start + 1)?;
        let (_, name_end) = super::library::skip_sexp_token(source, head_end)?;
        let span = super::library::skip_sexp_token(source, name_end)?;
        return Some((span, text));
    }
    None
}

/// Why KiCad could not use `name` for a footprint and its file, if it cannot.
fn name_reason(name: &str) -> Result<(), String> {
    if name.is_empty() {
        return Err("a footprint must have a name".to_string());
    }
    if name.trim() != name {
        return Err(
            "it has spaces at either end, which KiCad's rename dialog trims; pass the name \
             without them"
                .to_string(),
        );
    }
    if let Some(found) = name.chars().find(|c| INVALID_CHARACTERS.contains(*c)) {
        return Err(format!(
            "KiCad does not accept {} in a footprint name",
            shown(found)
        ));
    }
    if cfg!(windows) {
        if let Some(found) = name
            .chars()
            .find(|c| WINDOWS_INVALID_CHARACTERS.contains(*c) || c.is_control())
        {
            return Err(format!(
                "a Windows file name cannot hold {}, so KiCad could not save the footprint",
                shown(found)
            ));
        }
        let device = name.split('.').next().unwrap_or(name).trim_end();
        if WINDOWS_RESERVED_NAMES
            .iter()
            .any(|reserved| reserved.eq_ignore_ascii_case(device))
        {
            return Err(format!(
                "Windows reserves '{device}' as a device name, so it cannot be a file name"
            ));
        }
    }
    Ok(())
}

fn shown(character: char) -> String {
    match character {
        '\t' => "a tab".to_string(),
        '\n' | '\r' => "a line break".to_string(),
        other => format!("'{other}'"),
    }
}

/// Whether two names are one file on a case-insensitive filesystem.
fn same_ignoring_case(a: &str, b: &str) -> bool {
    a.to_lowercase() == b.to_lowercase()
}

fn is_pretty(path: &Path) -> bool {
    path.extension()
        .and_then(|extension| extension.to_str())
        .is_some_and(|extension| {
            if cfg!(windows) {
                extension.eq_ignore_ascii_case("pretty")
            } else {
                extension == "pretty"
            }
        })
}

/// The footprint names in the library: its `.kicad_mod` file stems.
fn footprint_names(library: &Path) -> anyhow::Result<Vec<String>> {
    let mut names = Vec::new();
    for entry in std::fs::read_dir(library)? {
        let path: PathBuf = entry?.path();
        let is_footprint = path
            .extension()
            .and_then(|extension| extension.to_str())
            .is_some_and(|extension| extension.eq_ignore_ascii_case("kicad_mod"));
        if let (true, Some(stem)) = (is_footprint, path.file_stem().and_then(|s| s.to_str())) {
            names.push(stem.to_string());
        }
    }
    Ok(names)
}

fn unusable(path: &Path, reason: &str) -> CallToolResult {
    let message = format!("Cannot rename in '{}': {reason}", path.display());
    CallToolResult::error_kind(
        crate::mcp::error::ToolErrorKind::HandlerError {
            reason: message.clone(),
        },
        message,
    )
}

fn answer_body(
    library: &Path,
    old_name: &str,
    new_name: &str,
    renamed: &Renamed,
) -> serde_json::Value {
    let mut body = json!({
        "success": true,
        "library_path": library.display().to_string(),
        "previous_name": old_name,
        "name": new_name,
        "footprint_path": library.join(format!("{new_name}.kicad_mod")).display().to_string(),
        "kept_references": format!(
            "Symbols whose Footprint field names '{old_name}', and footprints placed on boards, \
             still name it, as after a rename in KiCad. Update a symbol's Footprint field with \
             set_symbol_properties."
        ),
    });
    if let Some((previous, current)) = &renamed.value {
        body["value"] = json!({ "previous": previous, "current": current });
    }
    body
}

fn answer(library: &Path, old_name: &str, new_name: &str, renamed: &Renamed) -> CallToolResult {
    CallToolResult::json(&answer_body(library, old_name, new_name, renamed))
}

#[cfg(test)]
#[path = "footprint_rename_tests.rs"]
mod tests;
