//! Library symbol, footprint, and 3D model listing, search, and inspection tools.

use super::lib_tables::{
    expand_lib_uri, global_fp_lib_table, global_sym_lib_table, read_flat_lib_table,
    read_global_lib_table, resolve_footprint_path, KiCadSymbolSource,
};
use crate::mcp::error::ToolErrorKind;
use crate::mcp::protocol::CallToolResult;
use crate::tools::{get_path, require_str, ToolContext};
use copperpour_schematic_editor::types::fmt_f64;
use copperpour_sexp::parser::{parse_sexp, SexpNode};
use serde_json::json;
use std::path::{Path, PathBuf};

/// Extract the names of every top-level symbol defined in a `.kicad_sym`
/// library body, sorted and de-duplicated.
///
/// KiCad writes these files with CRLF line endings (on Windows) and TAB
/// indentation, so a fixed string search such as `\n  (symbol "` does not work
/// — it returned 0 symbols for every real library (KiCad 10, format version
/// 20251024). Instead we parse the S-expression structurally and read the
/// **direct** children of the `(kicad_symbol_lib …)` root whose head is
/// `symbol`. Nested unit sub-symbols (`NAME_0_1`, `NAME_1_1`, …) live one
/// level deeper, so they are excluded automatically — no name-pattern
/// heuristics required, and names containing underscores are preserved
/// verbatim.
pub(super) fn top_level_symbol_names(content: &str) -> anyhow::Result<Vec<String>> {
    let root = parse_sexp(content)
        .map_err(|e| anyhow::anyhow!("failed to parse .kicad_sym library: {e}"))?;
    let mut names: Vec<String> = root
        .find_all("symbol")
        .into_iter()
        .filter_map(|sym| sym.get(1).and_then(|n| n.as_str()).map(str::to_owned))
        .collect();
    names.sort();
    names.dedup();
    Ok(names)
}

/// The `.kicad_sym` file defining `nick:sym_name`, found exactly the way symbol
/// placement finds it — via [`KiCadSymbolSource`], so a lookup here and a
/// placement of the same lib_id can never disagree.
pub(super) fn resolve_symbol_lib_path(
    nick: &str,
    sym_name: &str,
    project_dir: Option<&Path>,
) -> Option<PathBuf> {
    use copperpour_schematic_editor::library::SymbolLibrarySource;
    KiCadSymbolSource::new(project_dir.map(Path::to_path_buf))
        .candidates(nick)
        .iter()
        .find_map(|candidate| {
            copperpour_schematic_editor::library::symbol_file_in(candidate, sym_name)
        })
}

/// Recursively collect every descendant `SexpNode::List` whose head matches
/// `head` (depth-first, document order). Pins live inside nested unit
/// sub-symbols `(symbol "NAME_N_M" …)`, not as direct children of the top-level
/// symbol, so a direct-children lookup is not enough.
fn descendants_with_head<'a>(node: &'a SexpNode, head: &str) -> Vec<&'a SexpNode> {
    fn walk<'a>(node: &'a SexpNode, head: &str, out: &mut Vec<&'a SexpNode>) {
        for child in node.children().unwrap_or(&[]) {
            if child.head() == Some(head) {
                out.push(child);
            }
            walk(child, head, out);
        }
    }
    let mut out = Vec::new();
    walk(node, head, &mut out);
    out
}

/// A library symbol's pin, with the unit and body style its unit sub-symbol
/// (`NAME_<unit>_<body style>`) gives it; `None` for a unit whose name KiCad
/// would not read.
#[derive(Debug)]
pub(super) struct SymbolPin<'a> {
    pub(super) node: &'a SexpNode,
    pub(super) unit: Option<u32>,
    pub(super) body_style: Option<u32>,
}

impl SymbolPin<'_> {
    pub(super) fn number(&self) -> &str {
        self.node
            .find("number")
            .and_then(|n| n.get(1))
            .and_then(|n| n.as_str())
            .unwrap_or("")
    }

    pub(super) fn name(&self) -> &str {
        self.node
            .find("name")
            .and_then(|n| n.get(1))
            .and_then(|n| n.as_str())
            .unwrap_or("")
    }

    pub(super) fn position(&self) -> (f64, f64) {
        self.node
            .find("at")
            .and_then(|a| Some((a.get_f64(1)?, a.get_f64(2)?)))
            .unwrap_or((0.0, 0.0))
    }
}

/// The symbol whose pins `sym_node` shows. KiCad takes a derived symbol's
/// pins from the root of its `extends` chain (`LIB_SYMBOL::GetGraphicalPins`,
/// `eeschema/lib_symbol.cpp`, 10.0.8), found as `GetSafeRootSymbol` finds it:
/// the last symbol reached before a missing base or a cycle (#712).
fn pin_owner<'a>(root: &'a SexpNode, sym_node: &'a SexpNode) -> &'a SexpNode {
    let mut visited = std::collections::HashSet::new();
    let mut current = sym_node;
    while let Some(name) = current.get(1).and_then(|n| n.as_str()) {
        visited.insert(name);
        let Some(base_name) = current.find_str("extends") else {
            break;
        };
        let Some(base) = root
            .find_all("symbol")
            .into_iter()
            .find(|s| s.get(1).and_then(|n| n.as_str()) == Some(base_name))
        else {
            break;
        };
        if visited.contains(base_name) {
            break;
        }
        current = base;
    }
    current
}

/// Every pin `sym_node` shows, in file order (#712): those of its pin owner's
/// `NAME_<unit>_<body style>` units, and any written directly under it, which
/// KiCad reads as unit 1, body style 1 (as `edit_symbol_pin` reads them). No
/// pin is dropped: pins that share a number are each listed.
pub(super) fn resolve_symbol_pins<'a>(
    root: &'a SexpNode,
    sym_node: &'a SexpNode,
) -> Vec<SymbolPin<'a>> {
    let owner = pin_owner(root, sym_node);
    let mut pins = Vec::new();
    for child in owner.children().unwrap_or(&[]) {
        match child.head() {
            Some("pin") => pins.push(SymbolPin {
                node: child,
                unit: Some(1),
                body_style: Some(1),
            }),
            Some("symbol") => {
                let place = child.get(1).and_then(|n| n.as_str()).and_then(|name| {
                    let mut parts = name.rsplitn(3, '_');
                    let body_style = parts.next()?.parse().ok()?;
                    let unit = parts.next()?.parse().ok()?;
                    parts.next()?;
                    Some((unit, body_style))
                });
                for pin in descendants_with_head(child, "pin") {
                    pins.push(SymbolPin {
                        node: pin,
                        unit: place.map(|p| p.0),
                        body_style: place.map(|p| p.1),
                    });
                }
            }
            _ => {}
        }
    }
    pins
}

/// How many pins a pin number stands for, counted as KiCad counts it without
/// expanding (`CountStackedPinNotation`, `common/string_utils.cpp`, 10.0.8):
/// one per number of a stacked `[1,3,5-7]`, a run's length for a run, and 1
/// for anything else, invalid notation included. A run is never expanded, so
/// `[1-1000000000]` costs no more than `[1-3]`; KiCad sums into an `int`, this
/// into a `u64` (#712).
pub(super) fn stacked_pin_count(number: &str) -> u64 {
    crate::tools::symbol_pins::stacked(number)
        .iter()
        .map(|part| match part {
            crate::tools::symbol_pins::Stacked::One(_) => 1,
            // `stacked` keeps only runs with first <= last.
            crate::tools::symbol_pins::Stacked::Run(_, first, last) => {
                (i128::from(*last) - i128::from(*first) + 1) as u64
            }
        })
        .fold(0, u64::saturating_add)
}

/// The pin count KiCad gives a symbol (`LIB_SYMBOL::GetPinCount`): its pins in
/// every unit and in body style 1, a pin of body style 0 included, a stacked
/// number counting once per pin it stands for (#712).
fn kicad_pin_count(pins: &[SymbolPin]) -> u64 {
    pins.iter()
        .filter(|pin| matches!(pin.body_style, None | Some(0) | Some(1)))
        .map(|pin| stacked_pin_count(pin.number()))
        .fold(0, u64::saturating_add)
}

/// One element of a pin's number for the duplicate check, with what KiCad's
/// checker sorts it by after the number: body style, unit, and the pin.
type CheckedPin = (u32, u32, usize);

/// What KiCad's symbol checker reports as duplicate pins (`CheckDuplicatePins`,
/// `eeschema/symbol_checker.cpp`, 10.0.8): every pin's numbers, stacked ones
/// expanded, sorted by number, body style and unit; each neighbouring pair of
/// two pins with one number is reported unless both body styles are set and
/// differ. Units, positions and jumper settings make no difference (#712).
///
/// Runs are compared as intervals, never expanded: each prefix's numbers are
/// split where a run starts or ends, so the same pins hold every number of a
/// segment and KiCad's neighbouring pairs are the same throughout it; a pair
/// is reported once per segment, with the segment as a range. A number that
/// is not prefix-and-digits as KiCad writes it (no digits, a leading zero) can
/// only equal itself and is compared as text.
pub(super) fn duplicate_pin_warnings(pins: &[SymbolPin]) -> Vec<String> {
    use crate::tools::symbol_pins::{alphanumeric, stacked, Stacked};

    let mut runs: std::collections::BTreeMap<String, Vec<(i64, i64, CheckedPin)>> =
        std::collections::BTreeMap::new();
    let mut texts: std::collections::BTreeMap<String, Vec<CheckedPin>> =
        std::collections::BTreeMap::new();
    for (index, pin) in pins.iter().enumerate() {
        let (Some(unit), Some(body_style)) = (pin.unit, pin.body_style) else {
            continue;
        };
        let checked = (body_style, unit, index);
        for part in stacked(pin.number()) {
            match part {
                Stacked::Run(prefix, first, last) => {
                    runs.entry(prefix).or_default().push((first, last, checked));
                }
                Stacked::One(number) => match alphanumeric(&number) {
                    (prefix, Some(value)) if format!("{prefix}{value}") == number => {
                        runs.entry(prefix.to_string())
                            .or_default()
                            .push((value, value, checked));
                    }
                    _ => texts.entry(number).or_default().push(checked),
                },
            }
        }
    }

    let describe = |shown: &str, index: usize| {
        let pin = &pins[index];
        let shown = if pin.number() == shown {
            shown.to_string()
        } else {
            format!("{shown} ({})", pin.number())
        };
        let (x, y) = pin.position();
        format!(
            "pin {shown} '{}' at ({}, {}) in unit {}, body style {}",
            pin.name(),
            fmt_f64(x),
            fmt_f64(y),
            pin.unit.unwrap_or_default(),
            pin.body_style.unwrap_or_default()
        )
    };
    // (sort key, warning): sorted by the first number's text at the end, as
    // the expanded check sorted its numbers.
    let mut warnings: Vec<(String, String)> = Vec::new();
    let mut report = |key: String, shown: String, mut holders: Vec<CheckedPin>| {
        holders.sort_unstable();
        let what = if shown.contains('-') {
            "numbers"
        } else {
            "number"
        };
        for pair in holders.windows(2) {
            let ((style_a, _, a), (style_b, _, b)) = (pair[0], pair[1]);
            if a != b && !(style_a != 0 && style_b != 0 && style_a != style_b) {
                warnings.push((
                    key.clone(),
                    format!(
                        "duplicate pin {what} {shown}: {} and {}; KiCad's symbol checker reports them",
                        describe(&shown, a),
                        describe(&shown, b)
                    ),
                ));
            }
        }
    };
    for (number, holders) in texts {
        if holders.len() > 1 {
            report(number.clone(), number, holders);
        }
    }
    for (prefix, entries) in &runs {
        let mut edges: Vec<i128> = entries
            .iter()
            .flat_map(|(first, last, _)| [i128::from(*first), i128::from(*last) + 1])
            .collect();
        edges.sort_unstable();
        edges.dedup();
        for edge in edges.windows(2) {
            let (low, high) = (edge[0], edge[1] - 1);
            let holders: Vec<CheckedPin> = entries
                .iter()
                .filter(|(first, last, _)| i128::from(*first) <= low && high <= i128::from(*last))
                .map(|(_, _, checked)| *checked)
                .collect();
            if holders.len() < 2 {
                continue;
            }
            let shown = if low == high {
                format!("{prefix}{low}")
            } else {
                format!("{prefix}{low}-{prefix}{high}")
            };
            report(format!("{prefix}{low}"), shown, holders);
        }
    }
    warnings.sort_by(|a, b| a.0.cmp(&b.0));
    warnings.into_iter().map(|(_, warning)| warning).collect()
}

/// Search one library body for top-level symbols whose name contains `query`
/// (case-insensitive), returning result objects shaped like `search_symbols`.
pub(super) fn search_lib_symbols(
    nickname: &str,
    content: &str,
    query: &str,
) -> Vec<serde_json::Value> {
    let Ok(names) = top_level_symbol_names(content) else {
        return Vec::new();
    };
    names
        .into_iter()
        .filter(|n| n.to_lowercase().contains(query))
        .map(|sym_name| {
            json!({
                "library": nickname,
                "name": sym_name,
                "id": format!("{}:{}", nickname, sym_name)
            })
        })
        .collect()
}

pub(super) async fn handle_list_symbols_in_library(
    args: &serde_json::Value,
    _ctx: &ToolContext,
) -> anyhow::Result<CallToolResult> {
    let lib_path = get_path(args, "library_path")?;
    let limit = match crate::tools::response_limit(args, 100) {
        Ok(limit) => limit,
        Err(error) => return Ok(error),
    };
    let content = tokio::fs::read_to_string(&lib_path).await?;
    let symbols = top_level_symbol_names(&content)?;
    let bounded = crate::tools::BoundedList::new(&symbols, limit);

    Ok(CallToolResult::text(
        serde_json::to_string(&json!({
            "library": lib_path.to_str().unwrap_or(""),
            "count": symbols.len(),
            "shown": bounded.shown,
            "truncated": bounded.truncated,
            "symbols": bounded.items
        }))
        .unwrap(),
    ))
}

pub(super) async fn handle_search_symbols(
    args: &serde_json::Value,
    ctx: &ToolContext,
) -> anyhow::Result<CallToolResult> {
    let query = match require_str(args, "query") {
        Ok(v) => v.to_lowercase(),
        Err(e) => return Ok(e),
    };
    let limit = match crate::tools::response_limit(args, 50) {
        Ok(limit) => limit,
        Err(error) => return Ok(error),
    };

    let project_dir = args["project_dir"]
        .as_str()
        .map(PathBuf::from)
        .or_else(|| ctx.config.project_dir.clone());

    // Gather (nickname, path) entries from the global sym-lib-table and, when a
    // project dir is supplied, the project's own sym-lib-table too — this is
    // what makes project-attached libraries searchable. Nested `(type "Table")`
    // references are followed and `${KICAD*_DIR}` URIs expanded, so the
    // libraries KiCad ships are included.
    let mut entries: Vec<(String, String)> = Vec::new();
    let mut tables: Vec<PathBuf> = global_sym_lib_table().into_iter().collect();
    if let Some(pd) = &project_dir {
        tables.push(pd.join("sym-lib-table"));
    }
    for table in &tables {
        for lib in read_flat_lib_table(table) {
            if let (Some(nick), Some(path)) = (lib["nickname"].as_str(), lib["path"].as_str()) {
                entries.push((nick.to_string(), path.to_string()));
            }
        }
    }

    let results = search_symbol_entries(entries, &query, limit).await;
    Ok(search_response(&query, &results, limit))
}

/// Stop after one extra match: enough to prove truncation without scanning the
/// rest of the installed catalogue just to compute a total we never promised.
pub(super) async fn search_symbol_entries(
    entries: Vec<(String, String)>,
    query: &str,
    limit: usize,
) -> Vec<serde_json::Value> {
    let mut results = Vec::new();
    // `entries` holds resolved filesystem paths, not the raw uris they came
    // from — read_flat_lib_table does that expansion now.
    'outer: for (nickname, resolved) in entries {
        let lib_path = PathBuf::from(&resolved);
        if !lib_path.exists() {
            continue;
        }
        let Ok(lib_content) = tokio::fs::read_to_string(&lib_path).await else {
            continue;
        };
        for hit in search_lib_symbols(&nickname, &lib_content, query) {
            results.push(hit);
            if results.len() > limit {
                break 'outer;
            }
        }
    }

    results
}

pub(super) fn search_response(
    query: &str,
    results: &[serde_json::Value],
    limit: usize,
) -> CallToolResult {
    let bounded = crate::tools::BoundedList::new(results, limit);
    CallToolResult::json(&json!({
        "query": query,
        "count": bounded.shown,
        "shown": bounded.shown,
        "truncated": bounded.truncated,
        "results": bounded.items
    }))
}

pub(super) async fn handle_list_library_footprints(
    args: &serde_json::Value,
    _ctx: &ToolContext,
) -> anyhow::Result<CallToolResult> {
    let library_path_str = match require_str(args, "library_path") {
        Ok(v) => v,
        Err(e) => return Ok(e),
    };
    let lib_dir = PathBuf::from(library_path_str);

    if !lib_dir.is_dir() {
        return Ok(CallToolResult::error(format!(
            "Not a directory: {}",
            library_path_str
        )));
    }

    let mut footprints = Vec::new();
    let mut rd = tokio::fs::read_dir(&lib_dir).await?;
    while let Ok(Some(entry)) = rd.next_entry().await {
        let name = entry.file_name();
        let name_str = name.to_string_lossy();
        if name_str.ends_with(".kicad_mod") {
            footprints.push(name_str.trim_end_matches(".kicad_mod").to_string());
        }
    }
    footprints.sort();

    Ok(CallToolResult::text(
        serde_json::to_string(&json!({
            "library": library_path_str,
            "count": footprints.len(),
            "footprints": footprints
        }))
        .unwrap(),
    ))
}

pub(super) async fn handle_get_footprint_info(
    args: &serde_json::Value,
    _ctx: &ToolContext,
) -> anyhow::Result<CallToolResult> {
    let fp_path_str = match require_str(args, "footprint_path") {
        Ok(v) => v,
        Err(e) => return Ok(e),
    };

    // Resolve "Library:Footprint" against the project's fp-lib-table as well
    // as the global one, when the caller says which project they mean.
    let project_dir = args["project"]
        .as_str()
        .map(PathBuf::from)
        .and_then(|p| p.parent().map(Path::to_path_buf));
    let path = match resolve_footprint_path(fp_path_str, project_dir.as_deref()) {
        Ok(p) => p,
        Err(msg) => return Ok(CallToolResult::error(msg)),
    };

    let content = tokio::fs::read_to_string(&path).await?;
    let footprint = parse_sexp(&content)?;

    // KiCad controls indentation and line endings, so every field here is read
    // from the parsed footprint rather than inferred from source text — a
    // `descr` mentioning a pad must not be counted as one. The name is the
    // root node's first datum; pads, models and graphics are its direct
    // children.
    let description = footprint.find_str("descr").unwrap_or_default().to_string();
    let fp_name = footprint
        .get(1)
        .and_then(|node| node.as_str())
        .map(str::to_string)
        .unwrap_or_else(|| {
            path.file_stem()
                .unwrap_or_default()
                .to_string_lossy()
                .to_string()
        });
    let pad_count = footprint
        .children()
        .unwrap_or(&[])
        .iter()
        .filter(|node| node.head() == Some("pad"))
        .count();
    let has_courtyard = footprint
        .children()
        .unwrap_or(&[])
        .iter()
        .any(|node| matches!(node.find_str("layer"), Some("B.CrtYd" | "F.CrtYd")));
    let has_3d = footprint.find("model").is_some();

    let mut response = json!({
        "name": fp_name,
        "description": description,
        "pad_count": pad_count,
        "has_courtyard": has_courtyard,
        "has_3d_model": has_3d,
        "models": crate::tools::footprint_models::written_models(&footprint)
            .iter()
            .map(crate::tools::footprint_models::WrittenModel::to_json)
            .collect::<Vec<_>>(),
        "path": path.to_str().unwrap_or("")
    });
    if args["include_pads"].as_bool().unwrap_or(false) {
        let mut pads = Vec::with_capacity(pad_count);
        for pad in footprint
            .children()
            .unwrap_or(&[])
            .iter()
            .filter(|node| node.head() == Some("pad"))
        {
            let pad_number = pad
                .get(1)
                .and_then(SexpNode::as_str)
                .unwrap_or("<unreadable>");
            let Some(at) = pad.find("at") else {
                return Ok(CallToolResult::error(format!(
                    "Pad '{pad_number}' has no readable footprint-local position"
                )));
            };
            let (Some(x), Some(y)) = (at.get_f64(1), at.get_f64(2)) else {
                return Ok(CallToolResult::error(format!(
                    "Pad '{pad_number}' has an invalid footprint-local position"
                )));
            };
            let rotation_deg = at.get_f64(3).unwrap_or(0.0);
            let info = match crate::tools::pcb_components::pad_info_from_sexp(
                pad,
                x,
                y,
                rotation_deg,
                String::new(),
            ) {
                Ok(info) => info,
                Err(error) => {
                    return Ok(CallToolResult::error(format!(
                        "Could not read pad '{pad_number}': {error}"
                    )))
                }
            };
            let mut info = serde_json::to_value(info)?;
            // A custom pad's anchor, clearance and primitives, in the form
            // edit_footprint_pad and create_footprint take as custom_shape.
            if pad.get(3).and_then(SexpNode::as_str) == Some("custom") {
                match crate::tools::pad_primitives::CustomPadShape::from_pad(pad) {
                    Ok(custom) => info["custom_shape"] = custom.to_json(),
                    Err(reason) => info["custom_shape_unreadable"] = json!(reason),
                }
            }
            pads.push(info);
        }
        response["coordinate_space"] = json!("footprint_local");
        response["pads"] = json!(pads);
    }
    let graphics_layer = args["graphics_layer"].as_str();
    let include_graphics =
        args["include_graphics"].as_bool().unwrap_or(false) || graphics_layer.is_some();
    if include_graphics {
        let graphics =
            match crate::tools::footprint_graphics::inspect_graphics(&content, graphics_layer) {
                Ok(graphics) => graphics,
                Err(
                    crate::tools::footprint_graphics::FootprintGraphicsError::InvalidArgument {
                        field,
                        reason,
                    },
                ) => {
                    return Ok(CallToolResult::error_kind(
                        crate::mcp::error::ToolErrorKind::InvalidArgument {
                            field: field.clone(),
                            reason: reason.clone(),
                        },
                        format!("Argument '{field}' is invalid: {reason}"),
                    ));
                }
                Err(crate::tools::footprint_graphics::FootprintGraphicsError::Conflict(reason))
                | Err(crate::tools::footprint_graphics::FootprintGraphicsError::StaleTarget {
                    reason,
                    ..
                }) => {
                    return Ok(CallToolResult::error(reason));
                }
            };
        response["graphic_count"] = json!(graphics.len());
        response["graphics"] = serde_json::to_value(graphics)?;
    }

    Ok(CallToolResult::json(&response))
}

// ─── search_footprints (moved from verification toolset) ─────────────────────

pub(super) async fn handle_search_footprints(
    args: &serde_json::Value,
    _ctx: &ToolContext,
) -> anyhow::Result<CallToolResult> {
    let query = match require_str(args, "query") {
        Ok(v) => v.to_lowercase(),
        Err(e) => return Ok(e),
    };
    let limit = match crate::tools::response_limit(args, 50) {
        Ok(limit) => limit,
        Err(error) => return Ok(error),
    };

    let results =
        search_footprint_entries(read_global_lib_table(global_fp_lib_table()), &query, limit).await;
    Ok(search_response(
        args["query"].as_str().unwrap(),
        &results,
        limit,
    ))
}

pub(super) async fn search_footprint_entries(
    libraries: Vec<serde_json::Value>,
    query: &str,
    limit: usize,
) -> Vec<serde_json::Value> {
    // Walk global fp-lib-table
    let mut results = Vec::new();

    'outer: for lib in libraries {
        let nickname = lib["nickname"].as_str().unwrap_or("").to_string();
        let Some(dir) = lib["path"].as_str().map(PathBuf::from) else {
            continue;
        };
        let Ok(mut rd) = tokio::fs::read_dir(&dir).await else {
            continue;
        };
        while let Ok(Some(entry)) = rd.next_entry().await {
            let fname = entry.file_name();
            let fname_str = fname.to_string_lossy();
            let Some(fp_name) = fname_str.strip_suffix(".kicad_mod") else {
                continue;
            };
            if fp_name.to_lowercase().contains(query) {
                results.push(json!({
                    "library": nickname,
                    "name": fp_name,
                    "id": format!("{}:{}", nickname, fp_name)
                }));
                if results.len() > limit {
                    break 'outer;
                }
            }
        }
    }

    results
}

// ─── search_3d_models ────────────────────────────────────────────────────────

const MODEL_3D_EXTENSIONS: &[&str] = &[
    "step", "stp", "stpz", "stp.gz", "step.gz", "igs", "iges", "wrl", "wrz", "x3d", "idf", "emn",
];
const MAX_3D_MODEL_WALK_DEPTH: usize = 6;

pub(super) fn is_3d_model_file(path: &Path) -> bool {
    model_3d_format(path).is_some()
}

fn model_3d_format(path: &Path) -> Option<&'static str> {
    let name = path.file_name()?.to_str()?.to_ascii_lowercase();
    MODEL_3D_EXTENSIONS
        .iter()
        .copied()
        .find(|ext| name.ends_with(&format!(".{ext}")))
}

fn kicad_3dmodel_env_prefix(dir: &Path) -> String {
    for major in ["10", "9", "8"] {
        let var = format!("KICAD{major}_3DMODEL_DIR");
        if std::env::var_os(&var).is_some_and(|v| v == dir.as_os_str()) {
            return format!("${{{var}}}");
        }
    }
    dir.to_string_lossy().into_owned()
}

pub(super) fn collect_3d_models_in_dir(
    root: &Path,
    source: &str,
    prefix: &str,
    query_lower: &str,
    limit: usize,
    results: &mut Vec<serde_json::Value>,
) {
    if !root.is_dir() || results.len() > limit {
        return;
    }
    let mut stack: Vec<(PathBuf, usize)> = vec![(root.to_path_buf(), 0)];
    while let Some((dir, depth)) = stack.pop() {
        let Ok(rd) = std::fs::read_dir(&dir) else {
            continue;
        };
        let mut entries: Vec<_> = rd.filter_map(Result::ok).collect();
        entries.sort_by_key(|e| e.file_name());
        let mut subdirs = Vec::new();
        for entry in entries {
            let Ok(ft) = entry.file_type() else {
                continue;
            };
            if ft.is_symlink() {
                continue;
            }
            let path = entry.path();
            if ft.is_dir() {
                if depth < MAX_3D_MODEL_WALK_DEPTH {
                    subdirs.push((path, depth + 1));
                }
                continue;
            }
            if !ft.is_file() || !is_3d_model_file(&path) {
                continue;
            }
            let Ok(rel) = path.strip_prefix(root) else {
                continue;
            };
            let rel_slash = rel
                .components()
                .map(|c| c.as_os_str().to_string_lossy().into_owned())
                .collect::<Vec<_>>()
                .join("/");
            if !query_lower.is_empty() && !rel_slash.to_ascii_lowercase().contains(query_lower) {
                continue;
            }
            let name = path
                .file_stem()
                .map(|s| s.to_string_lossy().into_owned())
                .unwrap_or_default();
            let format = model_3d_format(&path).unwrap();
            let portable = format!("{prefix}/{rel_slash}");
            let project_dir = (source == "project").then(|| root.parent()).flatten();
            let kicad_path =
                if expand_lib_uri(&portable, project_dir).as_deref() == Some(path.as_path()) {
                    portable
                } else {
                    path.to_string_lossy().into_owned()
                };
            results.push(json!({
                "source": source,
                "name": name,
                "format": format,
                "kicad_path": kicad_path,
                "path": path.to_string_lossy(),
            }));
            if results.len() > limit {
                return;
            }
        }
        for sub in subdirs.into_iter().rev() {
            stack.push(sub);
        }
    }
}

pub(super) async fn handle_search_3d_models(
    args: &serde_json::Value,
    ctx: &ToolContext,
) -> anyhow::Result<CallToolResult> {
    let raw_query = match require_str(args, "query") {
        Ok(v) => v,
        Err(e) => return Ok(e),
    };
    let query_lower = raw_query.to_ascii_lowercase();
    let scope = match args.get("scope") {
        None | Some(serde_json::Value::Null) => "all",
        Some(serde_json::Value::String(s)) if matches!(s.as_str(), "all" | "kicad" | "project") => {
            s.as_str()
        }
        Some(_) => {
            return Ok(crate::tools::invalid_arg(
                "scope",
                "must be one of 'all', 'kicad', or 'project'",
            ));
        }
    };
    let limit = match crate::tools::response_limit(args, 50) {
        Ok(limit) => limit,
        Err(error) => return Ok(error),
    };
    let project_dir = match args.get("project_dir") {
        None | Some(serde_json::Value::Null) => ctx.config.project_dir.clone(),
        Some(serde_json::Value::String(s)) if !s.trim().is_empty() => Some(PathBuf::from(s)),
        Some(_) => {
            return Ok(crate::tools::invalid_arg(
                "project_dir",
                "must be a non-empty path string",
            ));
        }
    };

    if scope == "project" && project_dir.is_none() {
        return Ok(crate::tools::invalid_arg(
            "project_dir",
            "is required when scope is 'project' and no default project_dir is configured",
        ));
    }

    let mut results = Vec::new();
    if matches!(scope, "all" | "project") {
        if let Some(proj) = project_dir.as_deref() {
            let models_dir = proj.join("3dmodels");
            collect_3d_models_in_dir(
                &models_dir,
                "project",
                "${KIPRJMOD}/3dmodels",
                &query_lower,
                limit,
                &mut results,
            );
        }
    }
    if matches!(scope, "all" | "kicad") && results.len() <= limit {
        for dir in crate::tools::find_kicad_library_dirs("3dmodels") {
            let prefix = kicad_3dmodel_env_prefix(&dir);
            collect_3d_models_in_dir(&dir, "kicad", &prefix, &query_lower, limit, &mut results);
            if results.len() > limit {
                break;
            }
        }
    }

    Ok(search_response(raw_query, &results, limit))
}

// ─── get_symbol_info (moved from verification toolset) ───────────────────────

pub(super) async fn handle_get_symbol_info(
    args: &serde_json::Value,
    ctx: &ToolContext,
) -> anyhow::Result<CallToolResult> {
    let given = |name: &str| args.get(name).is_some_and(|value| !value.is_null());
    // The library, the symbol's name in it, how the library is named in
    // messages, and the keys that identify the symbol in the answer.
    let (lib_path, sym_name, lib_label, identity) = if given("library_path") {
        if given("lib_id") {
            return Ok(crate::tools::invalid_arg(
                "library_path",
                "name the symbol by lib_id, or by library_path with symbol_name, not both",
            ));
        }
        if given("project_dir") {
            return Ok(crate::tools::invalid_arg(
                "project_dir",
                "project_dir resolves a lib_id's library through the project's symbol table; \
                 library_path names the file itself, so it takes no project_dir",
            ));
        }
        let path = get_path(args, "library_path")?;
        let sym_name = match require_str(args, "symbol_name") {
            Ok(name) => name,
            Err(error) => return Ok(error),
        };
        if path.extension().and_then(|extension| extension.to_str()) != Some("kicad_sym") {
            return Ok(crate::tools::invalid_arg(
                "library_path",
                &format!("'{}' is not a .kicad_sym library file", path.display()),
            ));
        }
        if !path.is_file() {
            return Ok(CallToolResult::error_kind(
                ToolErrorKind::file_not_found(&path),
                format!("Symbol library not found: {}", path.display()),
            ));
        }
        let label = path.display().to_string();
        let identity = json!({ "library_path": label });
        (path, sym_name, label, identity)
    } else {
        if given("symbol_name") {
            return Ok(crate::tools::invalid_arg(
                "symbol_name",
                "symbol_name names a symbol in library_path; with lib_id, the symbol is its \
                 'Library:Symbol' suffix",
            ));
        }
        let lib_id = match require_str(args, "lib_id") {
            Ok(v) => v,
            Err(e) => return Ok(e),
        };

        let parts: Vec<&str> = lib_id.splitn(2, ':').collect();
        if parts.len() != 2 {
            return Ok(CallToolResult::error(
                "lib_id must be in 'Library:Symbol' format (e.g. 'Device:R')",
            ));
        }
        let (lib_nick, sym_name) = (parts[0], parts[1]);

        // Project dir is optional: an explicit arg wins, else the server default.
        let project_dir = args["project_dir"]
            .as_str()
            .map(PathBuf::from)
            .or_else(|| ctx.config.project_dir.clone());

        let lib_path = match resolve_symbol_lib_path(lib_nick, sym_name, project_dir.as_deref()) {
            Some(p) => p,
            None => {
                return Ok(CallToolResult::error(format!(
                    "Symbol '{}' not found: no library '{}' in the project or global \
                     sym-lib-table, nor in the installed KiCad symbol libraries",
                    lib_id, lib_nick
                )));
            }
        };
        let identity = json!({ "lib_id": lib_id, "library": lib_nick });
        (lib_path, sym_name, lib_nick.to_string(), identity)
    };
    let lib_nick = lib_label.as_str();

    let content = tokio::fs::read_to_string(&lib_path).await?;
    let root = parse_sexp(&content)
        .map_err(|e| anyhow::anyhow!("failed to parse .kicad_sym library '{lib_nick}': {e}"))?;

    // Top-level symbol with the exact name (the lib_id suffix). Nested unit
    // sub-symbols (NAME_N_M) are one level deeper, so they are skipped here.
    let sym_node = root
        .find_all("symbol")
        .into_iter()
        .find(|s| s.get(1).and_then(|n| n.as_str()) == Some(sym_name));
    let sym_node = match sym_node {
        Some(n) => n,
        None => {
            return Ok(CallToolResult::error(format!(
                "Symbol '{}' not found in library '{}'",
                sym_name, lib_nick
            )));
        }
    };

    // Every pin, each with its unit and body style; a derived symbol shows its
    // root's pins, as KiCad does (#712).
    let symbol_pins = resolve_symbol_pins(&root, sym_node);
    let pins: Vec<serde_json::Value> = symbol_pins
        .iter()
        .map(|pin| {
            let (px, py) = pin.position();
            json!({
                "number": pin.number(),
                "name": pin.name(),
                "type": pin.node.get(1).and_then(|n| n.as_str()).unwrap_or(""),
                "unit": pin.unit,
                "body_style": pin.body_style,
                "x": px,
                "y": py
            })
        })
        .collect();
    let mut warnings = duplicate_pin_warnings(&symbol_pins);

    // Properties are direct children of the top-level symbol.
    let mut properties = serde_json::Map::new();
    for prop in sym_node.find_all("property") {
        if let (Some(key), Some(val)) = (
            prop.get(1).and_then(|n| n.as_str()),
            prop.get(2).and_then(|n| n.as_str()),
        ) {
            properties.insert(key.to_string(), json!(val));
        }
    }

    let mut answer = identity;
    answer["name"] = json!(sym_name);
    answer["pin_count"] = json!(kicad_pin_count(&symbol_pins));
    answer["pins"] = json!(pins);
    answer["properties"] = json!(properties);
    // The drawing, item by item, and the revision set_symbol_graphics takes
    // to name one of them (#867).
    let (revision, graphics) = crate::tools::symbol_graphics::listed_graphics(&content, sym_name);
    answer["revision"] = json!(revision);
    match graphics {
        Ok(graphics) => answer["graphics"] = json!(graphics),
        Err(reason) => warnings.push(format!("the symbol's graphics are not listed: {reason}")),
    }
    if !warnings.is_empty() {
        answer["warnings"] = json!(warnings);
    }
    Ok(CallToolResult::text(
        serde_json::to_string(&answer).unwrap(),
    ))
}
