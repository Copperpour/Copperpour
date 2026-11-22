//! KiCad footprint and symbol library table resolution, listing, and registration.

use super::footprint_ops::{escape_library_string, invalid_library_argument};
use crate::mcp::protocol::CallToolResult;
use crate::tools::{get_path, require_str, ToolContext};
use copperpour_sexp::parser::parse_sexp;
use copperpour_sexp::writer::{
    apply_edits, find_balanced_block, find_block_starts, find_direct_child_blocks, read_consistent,
    write_atomic_if_unchanged, write_new_atomic, SexpEdit,
};
use serde_json::json;
use std::path::{Path, PathBuf};

/// Returns the path to the global fp-lib-table file, or `None` when the
/// user's home directory is unknown.
pub(super) fn global_fp_lib_table() -> Option<PathBuf> {
    crate::tools::kicad_config_dir().map(|dir| dir.join("fp-lib-table"))
}

/// Returns the path to the global sym-lib-table file, or `None` as above.
pub(super) fn global_sym_lib_table() -> Option<PathBuf> {
    crate::tools::kicad_config_dir().map(|dir| dir.join("sym-lib-table"))
}

/// What a global-table refusal names.
const GLOBAL_TABLE: &str = "KiCad's global library table";

/// The libraries in a global table, or none when there is no table path:
/// the same answer as a table that does not exist.
pub(super) fn read_global_lib_table(table: Option<PathBuf>) -> Vec<serde_json::Value> {
    table
        .map(|path| read_flat_lib_table(&path))
        .unwrap_or_default()
}

/// Directory of the structurally proven project that owns `file`, falling back
/// to the file's own directory when it belongs to no project — a loose
/// schematic keeps resolving against the tables beside it.
///
/// The project file is found by scanning for the extension rather than by name:
/// a sheet's filename says nothing about what the project is called.
///
/// A library table sitting beside `file` is the most specific answer. An
/// ancestor project is accepted only when its parsed root schematic reaches
/// this file. Unproven or multiple owners produce the existing structured
/// conflict, naming the schematic directory and all candidate roots (#189).
pub(crate) fn project_root_for(
    file: &Path,
) -> Result<Option<PathBuf>, crate::tools::SchematicTargetError> {
    project_root_for_from(file, std::env::current_dir)
}

/// [`project_root_for`], with a relative `file`'s ancestors scanned from
/// `current_dir()`.
pub(super) fn project_root_for_from(
    file: &Path,
    current_dir: impl FnOnce() -> std::io::Result<PathBuf>,
) -> Result<Option<PathBuf>, crate::tools::SchematicTargetError> {
    let Some(start) = file.parent() else {
        return Ok(None);
    };
    if holds_lib_table(start) {
        return Ok(Some(start.to_path_buf()));
    }
    Ok(
        crate::tools::resolve_schematic_ownership_from(file, current_dir)?
            .and_then(|ownership| ownership.project_file.parent().map(Path::to_path_buf))
            .or_else(|| Some(start.to_path_buf())),
    )
}

/// Whether `dir` carries a library table of its own.
fn holds_lib_table(dir: &Path) -> bool {
    dir.join("sym-lib-table").is_file() || dir.join("fp-lib-table").is_file()
}

/// Symbol libraries resolved as KiCad does: project `sym-lib-table` (shadowing
/// same-nickname global entries), then global, then the conventional
/// `<nickname>.kicad_symdir` / `.kicad_sym` layout. Same order as
/// [`resolve_footprint_path`]; the last step covers a missing global table.
///
/// Both the flattened tables and the install dirs are memoised: placing one
/// component asks for candidates at least twice, and the default global table
/// is a `(type "Table")` indirection to ~200 bundled entries, each of which
/// re-probes the install roots when `${KICAD*_DIR}` is unset.
pub(crate) struct KiCadSymbolSource {
    project_dir: Option<PathBuf>,
    tables: std::sync::OnceLock<Vec<serde_json::Value>>,
    install_dirs: std::sync::OnceLock<Vec<PathBuf>>,
}

impl KiCadSymbolSource {
    pub(crate) fn new(project_dir: Option<PathBuf>) -> Self {
        Self {
            project_dir,
            tables: std::sync::OnceLock::new(),
            install_dirs: std::sync::OnceLock::new(),
        }
    }

    /// For a `.kicad_sch` or `.kicad_pcb` — `sym-lib-table` sits at the project
    /// root, which is the nearest ancestor holding a `.kicad_pro`. A
    /// hierarchical sheet under `<proj>/sheets/` therefore still resolves
    /// against `<proj>/sym-lib-table`: KiCad anchors `KIPRJMOD` at the project,
    /// not at the sheet.
    pub(crate) fn for_file(file: &Path) -> Result<Self, crate::tools::SchematicTargetError> {
        Ok(Self::new(project_root_for(file)?))
    }

    /// Project entries first so they shadow same-nickname global ones.
    fn tables(&self) -> &[serde_json::Value] {
        self.tables.get_or_init(|| {
            let mut libs = Vec::new();
            if let Some(dir) = &self.project_dir {
                libs.extend(read_flat_lib_table(&dir.join("sym-lib-table")));
            }
            libs.extend(read_global_lib_table(global_sym_lib_table()));
            libs
        })
    }
}

impl copperpour_schematic_editor::library::SymbolLibrarySource for KiCadSymbolSource {
    fn candidates(&self, nickname: &str) -> Vec<PathBuf> {
        let mut out = Vec::new();

        for lib in self
            .tables()
            .iter()
            .filter(|l| l["nickname"].as_str() == Some(nickname))
        {
            if let Some(path) = lib["path"].as_str() {
                out.push(PathBuf::from(path));
            }
        }

        let install_dirs = self
            .install_dirs
            .get_or_init(|| crate::tools::find_kicad_library_dirs("symbols"));
        for base in install_dirs {
            out.push(base.join(format!("{}.kicad_symdir", nickname)));
            out.push(base.join(format!("{}.kicad_sym", nickname)));
        }
        out
    }
}

/// Parse a lib-table S-expression and return list of (nickname, uri, type) tuples.
///
/// Indentation-agnostic: KiCad's own writers emit tab-indented, CRLF-terminated
/// tables while this crate's writer uses two spaces, so a fixed literal such as
/// `"\n  (lib "` silently matches nothing in a real `fp-lib-table`.
///
/// Each entry is located textually and then *parsed*, so a field is read from
/// the tree rather than scraped out of the block: an escaped quote inside a
/// `descr` no longer truncates the value, and a field KiCad wrote across a line
/// break is still found. Locating entries textually keeps a malformed one from
/// discarding the rest of the table.
pub(super) fn parse_lib_table(content: &str) -> Vec<serde_json::Value> {
    let mut libs = Vec::new();
    // Each entry: (lib (name "NICK") (type "...") (uri "...") (options "") (descr "..."))
    for start in find_block_starts(content, "lib") {
        let Some((block_start, block_end)) = find_balanced_block(content, start) else {
            continue;
        };
        let block = &content[block_start..block_end];
        let Ok(entry) = parse_sexp(block) else {
            tracing::warn!("lib-table entry at byte {block_start} does not parse — skipped");
            continue;
        };

        let field = |tag: &str| entry.find_str(tag).unwrap_or_default().to_string();

        libs.push(json!({
            "nickname": field("name"),
            "uri": field("uri"),
            "type": field("type"),
            "description": field("descr")
        }));
    }
    libs
}

/// Resolve a lib-table URI to a concrete path, expanding a leading
/// `${KICAD*_DIR}` reference.
///
/// KiCad's shipped tables address bundled libraries as
/// `${KICAD10_FOOTPRINT_DIR}/Resistor_SMD.pretty`. An exported environment
/// variable wins; otherwise the variable's kind is inferred from its name and
/// the known install locations are searched.
/// User path variables from `kicad_common.json` (Preferences → Configure
/// Paths).
///
/// These are KiCad's own variables, not process environment variables — KiCad
/// stores them in its config and substitutes them itself when reading a
/// lib-table, so `std::env::var` never finds them. A table written against one
/// is perfectly normal and is the recommended way to keep a table portable
/// across machines.
fn kicad_user_path_vars() -> std::collections::HashMap<String, String> {
    let Some(Ok(text)) = crate::tools::kicad_config_dir()
        .map(|dir| std::fs::read_to_string(dir.join("kicad_common.json")))
    else {
        return std::collections::HashMap::new();
    };
    let Ok(json) = serde_json::from_str::<serde_json::Value>(&text) else {
        return std::collections::HashMap::new();
    };
    json.get("environment")
        .and_then(|e| e.get("vars"))
        .and_then(|v| v.as_object())
        .map(|m| {
            m.iter()
                .filter_map(|(k, v)| v.as_str().map(|s| (k.clone(), s.to_string())))
                .collect()
        })
        .unwrap_or_default()
}

pub(crate) fn expand_lib_uri(uri: &str, kiprjmod: Option<&Path>) -> Option<PathBuf> {
    let Some(rest) = uri.strip_prefix("${") else {
        return (!uri.is_empty()).then(|| PathBuf::from(uri));
    };
    let close = rest.find('}')?;
    let var = &rest[..close];
    let tail = rest[close + 1..].trim_start_matches(['/', '\\']);

    // ${KIPRJMOD} is the project directory — resolved from the table's own
    // location, not the environment: KiCad sets it per open project at
    // runtime, so an exported value (if any) may belong to a different
    // project than the table being read. Project-scoped registrations are
    // the default for register_footprint_library, so this is the common
    // case for user-registered libraries, not an edge.
    if var == "KIPRJMOD" {
        let p = kiprjmod?.join(tail);
        return p.exists().then_some(p);
    }

    // var_os, not var: `var` treats a non-Unicode value as absent, which would
    // send a perfectly good ${KICAD*_DIR} down the install-root guess path.
    if let Some(base) = std::env::var_os(var) {
        let p = PathBuf::from(base).join(tail);
        if p.exists() {
            return Some(p);
        }
    }

    // User path variables (Preferences → Configure Paths) are stored in
    // kicad_common.json and are NOT process environment variables, so the
    // var_os lookup above can never see them. They are the normal way to write
    // a portable lib-table — `${MY_PARTS}/house.kicad_sym` — and without this
    // every such entry resolved to None and the library was invisible.
    // Credit to @JYPochez (#172) for finding this.
    if let Some(base) = kicad_user_path_vars().get(var) {
        let p = PathBuf::from(base).join(tail);
        if p.exists() {
            return Some(p);
        }
    }

    // e.g. KICAD10_FOOTPRINT_DIR -> "footprints"
    let kind = if var.ends_with("_FOOTPRINT_DIR") {
        "footprints"
    } else if var.ends_with("_SYMBOL_DIR") {
        "symbols"
    } else if var.ends_with("_3DMODEL_DIR") {
        "3dmodels"
    } else if var.ends_with("_TEMPLATE_DIR") {
        "template"
    } else {
        return None;
    };

    crate::tools::find_kicad_library_dirs(kind)
        .into_iter()
        .map(|base| base.join(tail))
        .find(|p| p.exists())
}

/// Maximum depth when following nested `(type "Table")` lib-table references.
const MAX_LIB_TABLE_DEPTH: usize = 4;

/// Parse a lib-table and return concrete libraries, following nested tables.
///
/// KiCad 10 no longer copies its ~155 bundled libraries into the user's table.
/// The default global table instead holds a single indirection entry —
/// `(lib (name "KiCad") (type "Table") (uri ".../template/fp-lib-table"))` —
/// pointing at the shipped template table. Treating that entry as a library
/// makes every bundled library invisible, so it is followed here.
///
/// Each returned entry carries the original `uri` plus a resolved `path`
/// whenever [`expand_lib_uri`] yields one: a `${KICAD*_DIR}` URI resolves only
/// if the expansion exists on disk, while a plain URI is passed through as
/// written. The target may be a directory (`.pretty`) or a file
/// (`.kicad_sym`), so the presence of `path` is not a promise that the library
/// is readable — only that the URI was understood.
pub(super) fn flatten_lib_table(
    content: &str,
    depth: usize,
    kiprjmod: Option<&Path>,
) -> Vec<serde_json::Value> {
    let mut out = Vec::new();

    for mut entry in parse_lib_table(content) {
        let uri = entry["uri"].as_str().unwrap_or("").to_string();
        let is_nested = entry["type"].as_str() == Some("Table");

        if is_nested {
            if depth >= MAX_LIB_TABLE_DEPTH {
                tracing::warn!(
                    "lib-table nesting deeper than {} levels at '{}' — not followed",
                    MAX_LIB_TABLE_DEPTH,
                    uri
                );
                continue;
            }
            match expand_lib_uri(&uri, kiprjmod).map(std::fs::read_to_string) {
                Some(Ok(nested)) => out.extend(flatten_lib_table(&nested, depth + 1, kiprjmod)),
                _ => tracing::warn!("nested lib-table '{}' could not be read", uri),
            }
            continue;
        }

        if let Some(path) = expand_lib_uri(&uri, kiprjmod) {
            entry["path"] = json!(path.to_string_lossy());
        }
        out.push(entry);
    }

    out
}

/// Read a lib-table file from disk and flatten it, reporting a table that is
/// present but unreadable.
///
/// An absent table is normal and yields an empty list: a project without its
/// own fp-lib-table simply has none, and every caller checks both the global
/// and project tables. Anything else — a permissions problem, a truncated
/// file — is not normal, and must not be folded into the same empty list. The
/// symptom that produces is a bare `{"count": 0}`, which is precisely what the
/// bug this module fixes looked like, so silence here would make a real
/// failure indistinguishable from a regression.
pub(super) fn read_lib_table_checked(path: &Path) -> Result<Vec<serde_json::Value>, String> {
    match std::fs::read_to_string(path) {
        // ${KIPRJMOD} is the directory the project's lib-table lives in, so
        // the table's own parent IS the correct expansion base for a project
        // table. For the global table the parent is KiCad's config dir, where
        // a ${KIPRJMOD} entry would be authoring error to begin with — the
        // expansion then simply fails its exists() check.
        Ok(content) => Ok(flatten_lib_table(&content, 0, path.parent())),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Vec::new()),
        Err(e) => Err(format!("Cannot read lib-table {}: {}", path.display(), e)),
    }
}

/// As [`read_lib_table_checked`], for callers with nowhere to put an error.
///
/// The failure is logged rather than dropped in silence. Handlers that can
/// surface it to the user should call `read_lib_table_checked` directly.
pub(super) fn read_flat_lib_table(path: &Path) -> Vec<serde_json::Value> {
    match read_lib_table_checked(path) {
        Ok(libs) => libs,
        Err(msg) => {
            tracing::warn!("{msg}");
            Vec::new()
        }
    }
}

/// Whether a footprint reference is KiCad's `Library:Footprint` form rather
/// than a filesystem path.
///
/// "Contains a colon" is not enough, because Windows paths contain one too.
/// `C:\libs\R.kicad_mod` is caught by the separator test, but the
/// drive-*relative* form `C:R.kicad_mod` — meaning `R.kicad_mod` in the current
/// directory of drive C — carries no separator and is otherwise shaped exactly
/// like a lib id.
///
/// A one-letter prefix is therefore read as a drive letter rather than a
/// nickname. Nothing distinguishes the two, so this is a choice: a drive letter
/// is much the likelier reading, and guessing the other way means silently
/// hunting for a library named "C". The cost is that a single-letter nickname
/// cannot be written in this form — it is still reachable by path — and the
/// rule is applied on every platform so the behaviour does not change under
/// the caller's feet.
pub(crate) fn is_lib_id(reference: &str) -> bool {
    let Some((nick, _)) = reference.split_once(':') else {
        return false;
    };
    if reference.contains('/') || reference.contains('\\') {
        return false;
    }
    !(nick.len() == 1 && nick.as_bytes()[0].is_ascii_alphabetic())
}

/// The nickname the fp-lib-table gives to the library living in `dir`, if any.
///
/// This is the inverse of `resolve_footprint_path` and exists because a
/// nickname is *not* derivable from the directory name: KiCad lets a table map
/// any nickname to any path, so `MyParts` may well point at `vendor.pretty`,
/// and two nicknames may share one directory. Only the table can answer it.
///
/// Paths are compared canonicalised so a symlinked or non-normalised entry
/// still matches, falling back to a literal comparison when canonicalisation
/// fails (a directory that no longer exists, say).
pub(crate) fn footprint_lib_nickname_for_dir(dir: &Path) -> Option<String> {
    let canonical = std::fs::canonicalize(dir).ok();
    let same = |candidate: &Path| -> bool {
        match (&canonical, std::fs::canonicalize(candidate).ok()) {
            (Some(a), Some(b)) => a == &b,
            _ => candidate == dir,
        }
    };

    read_global_lib_table(global_fp_lib_table())
        .into_iter()
        .find(|lib| lib["path"].as_str().is_some_and(|p| same(Path::new(p))))
        .and_then(|lib| lib["nickname"].as_str().map(str::to_string))
}

/// Resolve a footprint reference to an on-disk `.kicad_mod` path.
///
/// Accepts either a direct filesystem path or KiCad's `Library:Footprint`
/// form. Returns a human-readable message on failure so callers can surface it
/// verbatim.
///
/// A lib id is looked up in `project_dir`'s fp-lib-table first, then the
/// global one, and finally the conventional `<nickname>.pretty` layout under
/// the bundled library directories. Project-first matches KiCad, where a
/// project entry shadows a global one of the same nickname, and it is the only
/// order that makes `register_footprint_library` useful — it writes to the
/// project table by default, so a global-only lookup cannot see anything it
/// registers. The `.pretty` fallback covers a stock install whose global
/// table is missing or unreadable.
///
/// Symbols resolve the same way and in the same order, via
/// [`KiCadSymbolSource`] and `resolve_symbol_lib_path`.
pub(crate) fn resolve_footprint_path(
    reference: &str,
    project_dir: Option<&Path>,
) -> Result<PathBuf, String> {
    if !is_lib_id(reference) {
        // Check here rather than leaving it to the caller's read: an unchecked
        // path reaches the reader as a bare io::Error, which surfaces as
        // "The system cannot find the file specified. (os error 2)" with no
        // mention of what was being looked for.
        let path = PathBuf::from(reference);
        if !path.is_file() {
            return Err(format!(
                "Footprint file not found: {}. Pass either a path to a .kicad_mod \
                 file or a Library:Footprint id (e.g. 'Resistor_SMD:R_0402').",
                path.display()
            ));
        }
        return Ok(path);
    }

    let (nick, fp_name) = reference.split_once(':').expect("checked above");
    let filename = format!("{fp_name}.kicad_mod");

    // Project table first: its entries shadow same-nickname global ones.
    let mut libs = Vec::new();
    if let Some(project) = project_dir.map(|d| d.join("fp-lib-table")) {
        libs.extend(read_flat_lib_table(&project));
    }
    libs.extend(read_global_lib_table(global_fp_lib_table()));

    if let Some(lib) = libs.iter().find(|l| l["nickname"].as_str() == Some(nick)) {
        let Some(dir) = lib["path"].as_str() else {
            return Err(format!(
                "Library '{}' has an unresolvable URI '{}'",
                nick,
                lib["uri"].as_str().unwrap_or("")
            ));
        };
        let path = PathBuf::from(dir).join(&filename);
        if !path.is_file() {
            return Err(format!(
                "Footprint '{}' not found in library '{}' (looked for {})",
                fp_name,
                nick,
                path.display()
            ));
        }
        return Ok(path);
    }

    // Not in any table — fall back to the conventional `<nickname>.pretty`
    // layout under the discovered KiCad library directories.
    let attempted: Vec<PathBuf> = crate::tools::find_kicad_library_dirs("footprints")
        .into_iter()
        .map(|base| base.join(format!("{nick}.pretty")).join(&filename))
        .collect();
    if let Some(path) = attempted.iter().find(|p| p.is_file()) {
        return Ok(path.clone());
    }

    let known: Vec<&str> = libs
        .iter()
        .filter_map(|l| l["nickname"].as_str())
        .take(12)
        .collect();
    let attempted_list = if attempted.is_empty() {
        "no KiCad library directories were found — set KICAD10_FOOTPRINT_DIR for a \
         non-standard install"
            .to_string()
    } else {
        format!(
            "also looked for {}",
            attempted
                .iter()
                .map(|p| p.display().to_string())
                .collect::<Vec<_>>()
                .join(", ")
        )
    };
    Err(format!(
        "Library '{}' not found in the project or global fp-lib-table ({} libraries known{}); {}",
        nick,
        libs.len(),
        if known.is_empty() {
            String::new()
        } else {
            format!(", e.g. {}", known.join(", "))
        },
        attempted_list
    ))
}

pub(crate) async fn handle_register_footprint_library(
    args: &serde_json::Value,
    _ctx: &ToolContext,
) -> anyhow::Result<CallToolResult> {
    let lib_path = get_path(args, "library_path")?;
    let nickname = match require_str(args, "nickname") {
        Ok(v) => v,
        Err(e) => return Ok(e),
    };
    let scope = args["scope"].as_str().unwrap_or("project");
    let replace_existing = match args.get("replace_existing") {
        None | Some(serde_json::Value::Null) => false,
        Some(value) => match value.as_bool() {
            Some(value) => value,
            None => {
                return Ok(invalid_library_argument(
                    "replace_existing",
                    "must be a boolean when supplied",
                ))
            }
        },
    };

    let (table_path, uri) = match lib_table_target(
        scope,
        args["project"].as_str(),
        &lib_path,
        global_fp_lib_table(),
        "fp-lib-table",
    ) {
        Ok(target) => target,
        Err(e) => return Ok(e),
    };

    let registration = match register_in_lib_table_with_policy(
        &table_path,
        nickname,
        &uri,
        "KiCad",
        replace_existing,
    )
    .await?
    {
        Ok(registration) => registration,
        Err(error) => return Ok(invalid_library_argument(&error.field, error.reason)),
    };

    Ok(CallToolResult::text(
        serde_json::to_string(&json!({
            "success": true,
            "nickname": nickname,
            "scope": scope,
            "table": table_path.to_str().unwrap_or(""),
            "uri": uri,
            "state": registration.state.as_str(),
            "repaired_table_root": registration.repaired_root
        }))
        .unwrap(),
    ))
}

pub(super) async fn handle_list_footprint_libraries(
    args: &serde_json::Value,
    _ctx: &ToolContext,
) -> anyhow::Result<CallToolResult> {
    let scope = args["scope"].as_str().unwrap_or("all");
    let mut all_libs = Vec::new();

    // A table that exists but cannot be read is reported rather than counted
    // as zero libraries — "0" is the symptom of the bug this PR fixes, so the
    // two must not look alike.
    if scope == "global" || scope == "all" {
        let table = match crate::tools::require_user_path(global_fp_lib_table(), GLOBAL_TABLE) {
            Ok(table) => table,
            Err(refusal) => return Ok(refusal),
        };
        let mut libs = match read_lib_table_checked(&table) {
            Ok(libs) => libs,
            Err(msg) => return Ok(CallToolResult::error(msg)),
        };
        for lib in &mut libs {
            lib["scope"] = json!("global");
        }
        all_libs.extend(libs);
    }

    if (scope == "project" || scope == "all") && args["project"].is_string() {
        let proj = PathBuf::from(args["project"].as_str().unwrap());
        let table = proj.parent().unwrap_or(Path::new(".")).join("fp-lib-table");
        let mut libs = match read_lib_table_checked(&table) {
            Ok(libs) => libs,
            Err(msg) => return Ok(CallToolResult::error(msg)),
        };
        for lib in &mut libs {
            lib["scope"] = json!("project");
        }
        all_libs.extend(libs);
    }

    Ok(CallToolResult::text(
        serde_json::to_string(&json!({
            "count": all_libs.len(),
            "libraries": all_libs
        }))
        .unwrap(),
    ))
}

/// A lib-table path written the way KiCad writes one: forward slashes on every
/// platform, and without the Windows verbatim prefix.
///
/// `canonicalize` on Windows returns `\\?\C:\…`. That prefix is an OS-level
/// escape for the 260-character limit, not part of the path — KiCad neither
/// writes nor expects it, and a table carrying one is not portable. Backslashes
/// are normalised for the same reason: a URI written on Windows has to still
/// resolve when the project is opened on Linux or macOS.
/// Credit to @anyn99 (#163), whose PR carried this and the lexical fallback
/// below.
pub(super) fn portable_uri(path: &Path) -> String {
    let raw = path.to_string_lossy();
    let stripped = raw.strip_prefix(r"\\?\").unwrap_or(&raw);
    stripped.replace('\\', "/")
}

/// A lib-table URI for `lib_path`: `${KIPRJMOD}/…` when it lives inside the
/// project, so the project survives being moved or cloned. Anything outside
/// keeps its absolute path.
pub(super) fn project_relative_uri(lib_path: &Path, table_dir: &Path) -> String {
    // An empty table dir must never reach `strip_prefix`: it succeeds and
    // returns the whole path, which would emit a `${KIPRJMOD}//abs/path` that
    // resolves nowhere on read.
    if table_dir.as_os_str().is_empty() {
        return portable_uri(lib_path);
    }

    let relative = |rel: &Path| format!("${{KIPRJMOD}}/{}", portable_uri(rel));

    // Canonicalising both sides is the accurate comparison — it resolves
    // symlinks, `..` and case differences — but it only works for paths that
    // already exist. Registering a library before creating it is normal, so a
    // failure here falls through to a lexical compare rather than giving up on
    // portability.
    if let (Ok(lib), Ok(dir)) = (
        std::fs::canonicalize(lib_path),
        std::fs::canonicalize(table_dir),
    ) {
        if let Ok(rel) = lib.strip_prefix(&dir) {
            return relative(rel);
        }
        // Canonicalised and genuinely outside the project.
        return portable_uri(&lib);
    }

    match lib_path.strip_prefix(table_dir) {
        Ok(rel) => relative(rel),
        Err(_) => portable_uri(lib_path),
    }
}

fn repository_relative_uri(
    lib_path: &Path,
    table_dir: &Path,
    project_path: &Path,
) -> Option<String> {
    if !project_path.is_file()
        || project_path
            .extension()
            .and_then(|extension| extension.to_str())
            != Some("kicad_pro")
    {
        return None;
    }
    let repository = table_dir
        .ancestors()
        .find(|ancestor| ancestor.join(".git").exists())?;
    let repository = std::fs::canonicalize(repository).ok()?;
    let table_dir = std::fs::canonicalize(table_dir).ok()?;
    let library = std::fs::canonicalize(lib_path).ok()?;
    if !table_dir.starts_with(&repository) || !library.starts_with(&repository) {
        return None;
    }
    let relative = lexical_relative_path(&table_dir, &library)?;
    Some(format!("${{KIPRJMOD}}/{}", portable_uri(&relative)))
}

fn lexical_relative_path(from: &Path, to: &Path) -> Option<PathBuf> {
    use std::path::Component;

    let from_components: Vec<_> = from.components().collect();
    let to_components: Vec<_> = to.components().collect();
    if from_components.first() != to_components.first() {
        return None;
    }
    let common = from_components
        .iter()
        .zip(&to_components)
        .take_while(|(left, right)| left == right)
        .count();
    if common == 0 {
        return None;
    }

    let mut relative = PathBuf::new();
    for component in &from_components[common..] {
        if matches!(component, Component::Normal(_)) {
            relative.push("..");
        }
    }
    for component in &to_components[common..] {
        relative.push(component.as_os_str());
    }
    Some(relative)
}

/// Which lib-table a `register_*` call writes to, and the URI it records.
///
/// Shared by the symbol and footprint registrars so the two cannot drift: both
/// need the same `${KIPRJMOD}` portability and the same empty-parent guard.
/// Resolve the caller's `project` argument to the directory whose library table
/// KiCad actually reads.
///
/// The schema documents a `.kicad_pro` path, but every other tool on this
/// surface takes a project directory and callers pass one. Taking `parent()`
/// unconditionally then lands one level *above* the project — a directory that
/// holds no `.kicad_pro` and that KiCad never consults. The write succeeds
/// there and reports `inserted`, so two projects sharing a parent end up
/// sharing one stray table that neither of them reads.
///
/// A path that is neither an existing directory nor recognisable as a file is
/// refused rather than guessed at, because guessing is what produced the stray
/// table.
fn project_table_dir(project: &str) -> Result<PathBuf, CallToolResult> {
    let path = PathBuf::from(project);
    if path.is_dir() {
        return Ok(path);
    }
    let looks_like_a_file =
        path.is_file() || path.extension().is_some_and(|ext| ext == "kicad_pro");
    if looks_like_a_file {
        // `Path::new("board.kicad_pro").parent()` is Some("") — an empty path,
        // not None — so the `.` default needs an explicit emptiness check.
        return Ok(path
            .parent()
            .filter(|parent| !parent.as_os_str().is_empty())
            .unwrap_or(Path::new("."))
            .to_path_buf());
    }
    Err(CallToolResult::error(format!(
        "'project' must be a .kicad_pro file or an existing project directory, got '{project}'"
    )))
}

pub(super) fn lib_table_target(
    scope: &str,
    project: Option<&str>,
    lib_path: &Path,
    global_table: Option<PathBuf>,
    table_filename: &str,
) -> Result<(PathBuf, String), CallToolResult> {
    if scope == "global" {
        return Ok((
            crate::tools::require_user_path(global_table, GLOBAL_TABLE)?,
            portable_uri(lib_path),
        ));
    }
    let Some(proj) = project else {
        return Err(CallToolResult::error(
            "For project scope, provide 'project' path to .kicad_pro file",
        ));
    };
    let table_dir = project_table_dir(proj)?;
    let uri = project_relative_uri(lib_path, &table_dir);
    let uri = if uri.starts_with("${KIPRJMOD}") {
        uri
    } else {
        repository_relative_uri(lib_path, &table_dir, Path::new(proj)).unwrap_or(uri)
    };
    Ok((table_dir.join(table_filename), uri))
}

pub(crate) async fn handle_register_symbol_library(
    args: &serde_json::Value,
    _ctx: &ToolContext,
) -> anyhow::Result<CallToolResult> {
    let lib_path = get_path(args, "library_path")?;
    let nickname = match require_str(args, "nickname") {
        Ok(v) => v,
        Err(e) => return Ok(e),
    };
    let scope = args["scope"].as_str().unwrap_or("project");
    let replace_existing = match args.get("replace_existing") {
        None | Some(serde_json::Value::Null) => false,
        Some(value) => match value.as_bool() {
            Some(value) => value,
            None => {
                return Ok(invalid_library_argument(
                    "replace_existing",
                    "must be a boolean when supplied",
                ))
            }
        },
    };

    let (table_path, uri) = match lib_table_target(
        scope,
        args["project"].as_str(),
        &lib_path,
        global_sym_lib_table(),
        "sym-lib-table",
    ) {
        Ok(target) => target,
        Err(e) => return Ok(e),
    };

    // Same contract as register_footprint_library: report which of
    // inserted/unchanged/updated happened, rather than a bare "success" that
    // cannot be told apart from a silent no-op on an existing nickname (#211).
    let registration = match register_in_lib_table_with_policy(
        &table_path,
        nickname,
        &uri,
        "KiCad",
        replace_existing,
    )
    .await?
    {
        Ok(registration) => registration,
        Err(error) => return Ok(invalid_library_argument(&error.field, error.reason)),
    };

    Ok(CallToolResult::text(
        serde_json::to_string(&json!({
            "success": true,
            "nickname": nickname,
            "scope": scope,
            "table": table_path.to_str().unwrap_or(""),
            "uri": uri,
            "state": registration.state.as_str(),
            "repaired_table_root": registration.repaired_root
        }))
        .unwrap(),
    ))
}

pub(super) async fn handle_list_symbol_libraries(
    args: &serde_json::Value,
    _ctx: &ToolContext,
) -> anyhow::Result<CallToolResult> {
    let scope = args["scope"].as_str().unwrap_or("all");
    let mut all_libs = Vec::new();

    // Same as the footprint listing: an unreadable table is an error, not a
    // zero count.
    if scope == "global" || scope == "all" {
        let table = match crate::tools::require_user_path(global_sym_lib_table(), GLOBAL_TABLE) {
            Ok(table) => table,
            Err(refusal) => return Ok(refusal),
        };
        let mut libs = match read_lib_table_checked(&table) {
            Ok(libs) => libs,
            Err(msg) => return Ok(CallToolResult::error(msg)),
        };
        for lib in &mut libs {
            lib["scope"] = json!("global");
        }
        all_libs.extend(libs);
    }

    if (scope == "project" || scope == "all") && args["project"].is_string() {
        let proj = PathBuf::from(args["project"].as_str().unwrap());
        let table = proj
            .parent()
            .unwrap_or(Path::new("."))
            .join("sym-lib-table");
        let mut libs = match read_lib_table_checked(&table) {
            Ok(libs) => libs,
            Err(msg) => return Ok(CallToolResult::error(msg)),
        };
        for lib in &mut libs {
            lib["scope"] = json!("project");
        }
        all_libs.extend(libs);
    }

    Ok(CallToolResult::text(
        serde_json::to_string(&json!({
            "count": all_libs.len(),
            "libraries": all_libs
        }))
        .unwrap(),
    ))
}

/// Root S-expression element for a lib-table file, decided by its filename:
/// `sym-lib-table` uses `sym_lib_table`, everything else (`fp-lib-table`)
/// uses `fp_lib_table`. Credit: first diagnosed in PR #54 (presire) — the
/// hardcoded `fp_lib_table` scaffold produced symbol tables KiCad rejects.
pub(super) fn table_root_element(table_path: &Path) -> &'static str {
    let is_sym = table_path
        .file_name()
        .and_then(|n| n.to_str())
        .is_some_and(|n| n.contains("sym"));
    if is_sym {
        "sym_lib_table"
    } else {
        "fp_lib_table"
    }
}

/// Correct a lib-table whose root element is the wrong kind, returning the
/// content and whether anything changed.
///
/// A `sym-lib-table` whose root says `(fp_lib_table` — or the reverse — is
/// rejected outright: *"Library table … has type FOOTPRINT but expected SYMBOL;
/// skipping"*. #54 stopped Konnect from **creating** such a file, but a table
/// an older build already wrote stays broken forever, because every later
/// registration appends into whatever root it finds. Repairing on write is what
/// makes the earlier fix reach existing projects.
///
/// Only a root element is rewritten: the wrong token has to be the first thing
/// in the file, so the same words appearing inside a `(descr …)` are left alone.
pub(super) fn repair_table_root(content: String, expected_root: &str) -> (String, bool) {
    let wrong_root = if expected_root == "sym_lib_table" {
        "fp_lib_table"
    } else {
        "sym_lib_table"
    };
    let opening = format!("({wrong_root}");
    match content.find(&opening) {
        Some(pos) if content[..pos].trim().is_empty() => {
            let repaired = format!(
                "{}({expected_root}{}",
                &content[..pos],
                &content[pos + opening.len()..]
            );
            (repaired, true)
        }
        _ => (content, false),
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum LibTableRegistrationState {
    Inserted,
    Unchanged,
    Updated,
}

impl LibTableRegistrationState {
    fn as_str(self) -> &'static str {
        match self {
            Self::Inserted => "inserted",
            Self::Unchanged => "unchanged",
            Self::Updated => "updated",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct LibTableRegistration {
    state: LibTableRegistrationState,
    repaired_root: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct LibTableRegistrationError {
    field: String,
    reason: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct PreparedLibTableRegistration {
    content: String,
    registration: LibTableRegistration,
}

/// Register without the replace policy, reporting only whether the table's
/// root element needed repairing.
///
/// Both handlers call `register_in_lib_table_with_policy` directly now that
/// each reports its own registration state (#211), so this is only the
/// narrower shape the root-repair tests are written against.
#[cfg(test)]
pub(super) async fn register_in_lib_table(
    table_path: &Path,
    nickname: &str,
    uri: &str,
    lib_type: &str,
) -> anyhow::Result<bool> {
    let registration =
        register_in_lib_table_with_policy(table_path, nickname, uri, lib_type, false)
            .await?
            .map_err(|error| anyhow::anyhow!(error.reason))?;
    Ok(registration.repaired_root)
}

async fn register_in_lib_table_with_policy(
    table_path: &Path,
    nickname: &str,
    uri: &str,
    lib_type: &str,
    replace_existing: bool,
) -> anyhow::Result<Result<LibTableRegistration, LibTableRegistrationError>> {
    let root = table_root_element(table_path);
    let existing = table_path.exists();
    let source = if existing {
        read_consistent(table_path)?
    } else {
        format!("({root}\n  (version 7)\n)\n")
    };
    let prepared = match prepare_lib_table_registration(
        &source,
        root,
        nickname,
        uri,
        lib_type,
        replace_existing,
    ) {
        Ok(prepared) => prepared,
        Err(error) => return Ok(Err(error)),
    };
    if prepared.registration.repaired_root {
        tracing::warn!(
            "Repaired the root element of {} — it declared the wrong table kind, so KiCad was skipping the whole table",
            table_path.display()
        );
    }

    if prepared.content != source {
        if let Some(parent) = table_path.parent() {
            tokio::fs::create_dir_all(parent).await?;
        }
        if existing {
            persist_lib_table_registration(table_path, &source, &prepared.content)?;
        } else {
            write_new_atomic(table_path, &prepared.content)?;
        }
    }
    Ok(Ok(prepared.registration))
}

fn prepare_lib_table_registration(
    source: &str,
    expected_root: &str,
    nickname: &str,
    uri: &str,
    lib_type: &str,
    replace_existing: bool,
) -> Result<PreparedLibTableRegistration, LibTableRegistrationError> {
    let (content, repaired_root) = repair_table_root(source.to_string(), expected_root);
    let parsed = parse_sexp(&content).map_err(|error| LibTableRegistrationError {
        field: "table".to_string(),
        reason: format!("invalid library table S-expression: {error}"),
    })?;
    if parsed.head() != Some(expected_root) {
        return Err(LibTableRegistrationError {
            field: "table".to_string(),
            reason: format!("root must be {expected_root}"),
        });
    }

    let mut matches = Vec::new();
    for (start, end) in find_direct_child_blocks(&content, expected_root) {
        let block = &content[start..end];
        let node = parse_sexp(block).map_err(|_| LibTableRegistrationError {
            field: "table".to_string(),
            reason: "contains a malformed direct child".to_string(),
        })?;
        if node.head() == Some("lib") && node.find_str("name") == Some(nickname) {
            matches.push((start, end));
        }
    }
    if matches.len() > 1 {
        return Err(LibTableRegistrationError {
            field: "nickname".to_string(),
            reason: format!("library table contains duplicate nickname '{nickname}'"),
        });
    }

    let (content, state) = if let Some((start, end)) = matches.first().copied() {
        if !replace_existing {
            (content, LibTableRegistrationState::Unchanged)
        } else {
            let block = &content[start..end];
            let current_uri = parse_sexp(block)
                .ok()
                .and_then(|node| node.find_str("uri").map(str::to_string))
                .ok_or_else(|| LibTableRegistrationError {
                    field: "table".to_string(),
                    reason: format!("entry '{nickname}' has no valid uri"),
                })?;
            if current_uri == uri {
                (content, LibTableRegistrationState::Unchanged)
            } else {
                let uri_range = find_direct_child_blocks(block, "lib")
                    .into_iter()
                    .find(|(child_start, child_end)| {
                        parse_sexp(&block[*child_start..*child_end])
                            .is_ok_and(|node| node.head() == Some("uri"))
                    })
                    .ok_or_else(|| LibTableRegistrationError {
                        field: "table".to_string(),
                        reason: format!("entry '{nickname}' has no uri block"),
                    })?;
                let replacement = format!("(uri {})", quote_lib_table_string(uri));
                let updated = apply_edits(
                    content,
                    vec![SexpEdit::replace(
                        start + uri_range.0,
                        start + uri_range.1,
                        replacement,
                    )],
                );
                (updated, LibTableRegistrationState::Updated)
            }
        }
    } else {
        let root_end = find_balanced_block(&content, 0)
            .map(|range| range.1 - 1)
            .ok_or_else(|| LibTableRegistrationError {
                field: "table".to_string(),
                reason: "unbalanced library table root".to_string(),
            })?;
        let entry = format!(
            "\n  (lib (name {}) (type {}) (uri {}) (options \"\") (descr \"\"))",
            quote_lib_table_string(nickname),
            quote_lib_table_string(lib_type),
            quote_lib_table_string(uri)
        );
        (
            apply_edits(content, vec![SexpEdit::insert(root_end, entry)]),
            LibTableRegistrationState::Inserted,
        )
    };

    parse_sexp(&content).map_err(|error| LibTableRegistrationError {
        field: "table".to_string(),
        reason: format!("updated library table does not parse: {error}"),
    })?;
    Ok(PreparedLibTableRegistration {
        content,
        registration: LibTableRegistration {
            state,
            repaired_root,
        },
    })
}

pub(crate) fn quote_lib_table_string(value: &str) -> String {
    format!("\"{}\"", escape_library_string(value))
}

pub(super) fn persist_lib_table_registration(
    table_path: &Path,
    expected: &str,
    replacement: &str,
) -> Result<(), copperpour_sexp::SexpError> {
    write_atomic_if_unchanged(table_path, expected, replacement)
}
