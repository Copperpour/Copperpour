//! `rename_symbol`: rename a symbol in its `.kicad_sym` library, as KiCad's
//! symbol editor does (`SYMBOL_EDITOR_CONTROL::RenameSymbol`,
//! `eeschema/tools/symbol_editor_control.cpp`, 10.0.8).

use crate::mcp::protocol::CallToolResult;
use crate::tool;
use crate::tools::footprint_metadata::quote_string;
use crate::tools::symbol_fields::{header, symbol_block, unusable, write_checked};
use crate::tools::{invalid_arg, ToolContext, ToolDef};
use copperpour_sexp::writer::find_direct_child_blocks;
use copperpour_sexp::{apply_edits, parse_sexp, SexpEdit, SexpError, SexpNode};
use serde_json::json;
use std::collections::BTreeMap;

const OPERATION: &str = "rename_symbol";

pub(crate) fn tool() -> ToolDef {
    tool!(
        "rename_symbol",
        "Rename a symbol in its .kicad_sym library, as KiCad's symbol editor does: the symbol \
         and its unit blocks take the new name, symbols derived from it extend the new name, \
         and its Value changes too when it equals the old name. The new name is stored as \
         KiCad stores it, with \\ < > : \" escaped ({colon} for ':'); 'name' in the answer is \
         the stored name. Refuses a name already in the library. Only this library changes: \
         symbols placed in schematics keep the old library id, as in KiCad.",
        json!({
            "type": "object",
            "properties": {
                "library_path": {
                    "type": "string",
                    "description": "Path to the .kicad_sym library file"
                },
                "symbol_name": {
                    "type": "string",
                    "description": "The symbol's current name, as list_symbols_in_library lists it"
                },
                "new_name": {
                    "type": "string",
                    "description": "The new name, without spaces at either end"
                }
            },
            "required": ["library_path", "symbol_name", "new_name"],
            "additionalProperties": false
        }),
        |args, ctx| async move { handle_rename_symbol(args, ctx).await }
    )
}

async fn handle_rename_symbol(
    args: &serde_json::Value,
    _ctx: &ToolContext,
) -> anyhow::Result<CallToolResult> {
    let path = super::get_path(args, "library_path")?;
    let old_name = match super::require_str(args, "symbol_name") {
        Ok(name) => name,
        Err(error) => return Ok(error),
    };
    let requested = match super::require_str(args, "new_name") {
        Ok(name) => name,
        Err(error) => return Ok(error),
    };
    let new_name = match stored_name(requested) {
        Ok(name) => name,
        Err(reason) => return Ok(invalid_arg("new_name", &reason)),
    };
    if path.extension().and_then(|extension| extension.to_str()) != Some("kicad_sym") {
        return Ok(unusable(&path, "it does not end in .kicad_sym"));
    }
    if new_name == old_name {
        return Ok(invalid_arg(
            "new_name",
            "it is already the symbol's name, so there is nothing to rename",
        ));
    }
    let source = match copperpour_sexp::read_consistent(&path) {
        Ok(source) => source,
        Err(SexpError::Io(error)) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok(CallToolResult::error_kind(
                crate::mcp::error::ToolErrorKind::file_not_found(&path),
                format!("Symbol library not found: {}", path.display()),
            ));
        }
        Err(error) => return Err(error.into()),
    };
    if let Err(error) = symbol_block(&source, old_name) {
        return Ok(error);
    }
    if symbols(&source)
        .iter()
        .any(|symbol| symbol.name == new_name)
    {
        return Ok(invalid_arg(
            "new_name",
            &format!(
                "a symbol '{new_name}' is already in the library; rename_symbol does not \
                 overwrite one"
            ),
        ));
    }
    let renamed = match renamed_library(&source, old_name, &new_name) {
        Ok(renamed) => renamed,
        Err(reason) => return Ok(unusable(&path, &reason)),
    };

    let check = |text: &str| readback_mismatch(text, old_name, &new_name, &renamed.derived);
    if let Some(refusal) = write_checked(&path, &source, renamed.text.clone(), OPERATION, check)? {
        return Ok(refusal);
    }

    let mut body = json!({
        "success": true,
        "library_path": path.display().to_string(),
        "previous_name": old_name,
        "name": new_name,
        "derived_symbols": renamed.derived,
        "kept_references": format!(
            "Symbols placed in schematics still name '{old_name}' in their library id, as after \
             a rename in KiCad."
        ),
    });
    if new_name != requested {
        body["escaped"] = json!(format!(
            "KiCad stores '{requested}' as '{new_name}'; use the stored name in a library id"
        ));
    }
    if let Some((previous, current)) = &renamed.value {
        body["value"] = json!({ "previous": previous, "current": current });
    }
    if renamed.moved {
        body["moved"] = json!(
            "the symbol moved to where KiCad saves it, ordered by inheritance depth and name"
        );
    }
    Ok(CallToolResult::json(&body))
}

/// The name as KiCad stores it from its rename dialog: trimmed, then escaped
/// with `EscapeString( …, CTX_LIBID )`. Or why it cannot be a symbol name.
fn stored_name(requested: &str) -> Result<String, String> {
    if requested.trim() != requested {
        return Err(
            "it has spaces at either end, which KiCad's rename dialog trims; pass the name \
             without them"
                .to_string(),
        );
    }
    if requested.contains('\t') {
        return Err("KiCad cannot read a symbol name with a tab back in a library id".to_string());
    }
    let mut stored = String::with_capacity(requested.len());
    for character in requested.chars() {
        match character {
            '\\' => stored.push_str("{backslash}"),
            '<' => stored.push_str("{lt}"),
            '>' => stored.push_str("{gt}"),
            ':' => stored.push_str("{colon}"),
            '"' => stored.push_str("{dblquote}"),
            '\n' | '\r' => {}
            other => stored.push(other),
        }
    }
    if stored.is_empty() {
        return Err("a symbol must have a name".to_string());
    }
    Ok(stored)
}

/// A top-level symbol of the library.
pub(super) struct Symbol {
    /// The block, from its `(`.
    pub(super) start: usize,
    end: usize,
    pub(super) name: String,
    pub(super) extends: Option<String>,
}

pub(super) fn symbols(source: &str) -> Vec<Symbol> {
    find_direct_child_blocks(source, "kicad_symbol_lib")
        .into_iter()
        .filter_map(|(start, end)| {
            let (head, _) = header(source, start)?;
            if head != "symbol" {
                return None;
            }
            let node = parse_sexp(&source[start..end]).ok()?;
            Some(Symbol {
                start,
                end,
                name: node.get(1).and_then(SexpNode::as_str)?.to_string(),
                extends: node.find_str("extends").map(str::to_string),
            })
        })
        .collect()
}

struct Renamed {
    text: String,
    /// Symbols whose `extends` now names the new name.
    derived: Vec<String>,
    /// The Value before and after, when the rename changed it.
    value: Option<(String, String)>,
    /// Whether the symbol moved to keep the library in KiCad's save order.
    moved: bool,
}

/// The library with `old` renamed to `new` as KiCad saves it, or why the
/// library cannot be renamed in.
fn renamed_library(source: &str, old: &str, new: &str) -> Result<Renamed, String> {
    let symbols = symbols(source);
    let target = symbols
        .iter()
        .find(|symbol| symbol.name == old)
        .ok_or_else(|| format!("no symbol named '{old}'"))?;

    // Each block as it will be written.
    let mut blocks: Vec<String> = Vec::with_capacity(symbols.len());
    let mut derived = Vec::new();
    let mut value = None;
    for symbol in &symbols {
        let block = &source[symbol.start..symbol.end];
        if symbol.start == target.start {
            let (text, changed_value) = renamed_block(block, old, new)?;
            value = changed_value;
            blocks.push(text);
        } else if symbol.extends.as_deref() == Some(old) {
            blocks.push(retargeted(block, new)?);
            derived.push(symbol.name.clone());
        } else {
            blocks.push(block.to_string());
        }
    }

    // KiCad saves by inheritance depth, then name. A library already in
    // that order keeps it; any other order is left as it is.
    let names_after: Vec<&str> = symbols
        .iter()
        .map(|symbol| {
            if symbol.start == target.start {
                new
            } else {
                symbol.name.as_str()
            }
        })
        .collect();
    let depths = depths(&symbols);
    let key = |index: usize, names: &[&str]| (depths[index], names[index].to_string());
    let names_before: Vec<&str> = symbols.iter().map(|symbol| symbol.name.as_str()).collect();
    let in_kicad_order =
        (1..symbols.len()).all(|index| key(index - 1, &names_before) < key(index, &names_before));
    let contiguous = symbols.windows(2).all(|pair| {
        source[pair[0].end..pair[1].start]
            .chars()
            .all(|c| c.is_ascii_whitespace())
    });
    let mut order: Vec<usize> = (0..symbols.len()).collect();
    if in_kicad_order && contiguous {
        order.sort_by_key(|&index| key(index, &names_after));
    }
    let moved = order.iter().enumerate().any(|(slot, &index)| slot != index);

    // Each slot keeps its whitespace; the blocks fill the slots in order.
    let mut text = String::with_capacity(source.len() + 64);
    let mut cursor = 0;
    for (slot, symbol) in symbols.iter().enumerate() {
        text.push_str(&source[cursor..symbol.start]);
        text.push_str(&blocks[order[slot]]);
        cursor = symbol.end;
    }
    text.push_str(&source[cursor..]);
    Ok(Renamed {
        text,
        derived,
        value,
        moved,
    })
}

/// The renamed symbol's own block: its name, its unit blocks' names, and its
/// Value when that equals the old name.
fn renamed_block(
    block: &str,
    old: &str,
    new: &str,
) -> Result<(String, Option<(String, String)>), String> {
    let mut edits = vec![SexpEdit::replace(
        name_span(block, 0).ok_or("its name cannot be read")?.0,
        name_span(block, 0).ok_or("its name cannot be read")?.1,
        quote_string(new),
    )];
    let mut value = None;
    for (start, end) in find_direct_child_blocks(block, "symbol") {
        let Some((head, _)) = header(block, start) else {
            continue;
        };
        let node = parse_sexp(&block[start..end]).map_err(|error| error.to_string())?;
        match head {
            "symbol" => {
                // KiCad reads a unit block as `<symbol name>_<unit>_<body style>`,
                // with `{slash}` read as '/' (`parseLibSymbol`).
                let unit = node
                    .get(1)
                    .and_then(SexpNode::as_str)
                    .ok_or("a unit block has no name")?
                    .replace("{slash}", "/");
                let suffix = unit
                    .strip_prefix(&old.replace("{slash}", "/"))
                    .filter(|suffix| suffix.starts_with('_'))
                    .ok_or_else(|| format!("its unit block '{unit}' is not named after it"))?;
                let (span_start, span_end) =
                    name_span(block, start).ok_or("a unit block's name cannot be read")?;
                edits.push(SexpEdit::replace(
                    span_start,
                    span_end,
                    quote_string(&format!("{new}{suffix}")),
                ));
            }
            "property" => {
                let is_value = node
                    .get(1)
                    .and_then(SexpNode::as_str)
                    .is_some_and(|name| name.eq_ignore_ascii_case("Value"));
                let text = node.get(2).and_then(SexpNode::as_str);
                if is_value && text == Some(old) {
                    let (_, name_end) =
                        name_span(block, start).ok_or("its Value cannot be read")?;
                    let (value_start, value_end) = super::library::skip_sexp_token(block, name_end)
                        .ok_or("its Value cannot be read")?;
                    edits.push(SexpEdit::replace(value_start, value_end, quote_string(new)));
                    value = Some((old.to_string(), new.to_string()));
                }
            }
            _ => {}
        }
    }
    Ok((apply_edits(block.to_string(), edits), value))
}

/// A derived symbol's block with its `extends` naming `new`.
fn retargeted(block: &str, new: &str) -> Result<String, String> {
    let (start, _) = find_direct_child_blocks(block, "symbol")
        .into_iter()
        .find(|&(start, _)| header(block, start).is_some_and(|(head, _)| head == "extends"))
        .ok_or("a derived symbol has no extends")?;
    let (span_start, span_end) = name_span(block, start).ok_or("an extends cannot be read")?;
    Ok(apply_edits(
        block.to_string(),
        vec![SexpEdit::replace(span_start, span_end, quote_string(new))],
    ))
}

/// The span of the first argument of the block opening at `start`.
fn name_span(source: &str, start: usize) -> Option<(usize, usize)> {
    let (_, head_end) = super::library::skip_sexp_token(source, start + 1)?;
    super::library::skip_sexp_token(source, head_end)
}

/// Each symbol's inheritance depth: 0 for a root, its parent's plus one for a
/// derived symbol (`LIB_SYMBOL::GetInheritanceDepth`).
fn depths(symbols: &[Symbol]) -> Vec<usize> {
    let parents: BTreeMap<&str, Option<&str>> = symbols
        .iter()
        .map(|symbol| (symbol.name.as_str(), symbol.extends.as_deref()))
        .collect();
    symbols
        .iter()
        .map(|symbol| {
            let mut depth = 0;
            let mut current = symbol.extends.as_deref();
            while let Some(parent) = current {
                depth += 1;
                if depth > symbols.len() {
                    break;
                }
                current = parents.get(parent).copied().flatten();
            }
            depth
        })
        .collect()
}

/// Why `written` is not the rename, or `None` when it is.
fn readback_mismatch(written: &str, old: &str, new: &str, derived: &[String]) -> Option<String> {
    let symbols = symbols(written);
    if symbols.iter().any(|symbol| symbol.name == old) {
        return Some(format!("symbol '{old}' is still in the library"));
    }
    if symbols.iter().filter(|symbol| symbol.name == new).count() != 1 {
        return Some(format!("symbol '{new}' is not in the library once"));
    }
    derived.iter().find_map(|name| {
        let found = symbols.iter().find(|symbol| &symbol.name == name)?;
        (found.extends.as_deref() != Some(new))
            .then(|| format!("symbol '{name}' does not extend '{new}'"))
    })
}

#[cfg(test)]
#[path = "symbol_rename_tests.rs"]
mod tests;
