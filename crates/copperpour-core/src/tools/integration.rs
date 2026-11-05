//! `integration` toolset — JLCPCB parts database, datasheet enrichment, and Freerouting.
//!
//! JLCPCB tools query a local SQLite cache of the JLCPCB parts database.
//! Datasheet enrichment uses the LCSC HTTP API. The Freerouting tools are
//! registered here, and their discovery and routing code lives in
//! `crate::autorouting::freerouting` (#209).
//!
//! The three network calls (JLCPCB database download, LCSC datasheet lookups)
//! go through `get_with_backoff`, which retries transient failures (network
//! errors, 429, 5xx) with exponential backoff before giving up.
//!
//! The three JLCPCB query tools (`search_jlcpcb_parts`, `get_jlcpcb_part`,
//! `suggest_jlcpcb_alternatives`) cache results in `ToolContext::jlcpcb_cache`
//! (5-minute TTL) to avoid re-running an identical SQLite query for repeated
//! lookups within a session. Responses carry a `"cached"` field so callers
//! can see whether a given result came from cache.

use crate::autorouting::freerouting::{self, freerouting_jar_path_description};
// The catalog and the LCSC API moved to `sourcing` (#209); the handlers here
// call them as they did when they lived in this file.
use crate::mcp::{error::ToolErrorKind, protocol::CallToolResult};
use crate::sourcing::catalog::*;
use crate::sourcing::lcsc_api::*;
use crate::tool;
use crate::tools::{get_path, lcsc_packages, require_str, ToolContext, ToolDef};
use copperpour_sexp::{
    command::{commit_command, prepare_command, ItemId, SchematicCommand},
    parse_sexp,
    writer::{find_direct_child_blocks, read_consistent},
    SexpError, SexpNode,
};
use serde_json::json;
use std::collections::HashMap;
use std::path::{Path, PathBuf};

// ─── Tool definitions ─────────────────────────────────────────────────────────

pub fn tools() -> Vec<ToolDef> {
    vec![
        tool!(
            "download_jlcpcb_database",
            "Download or update the local JLCPCB component parts database cache (SQLite).",
            json!({
                "type": "object",
                "properties": {
                    "output_path": { "type": "string", "description": "Local path to store the SQLite database file (optional, uses config default)" },
                    "force": { "type": "boolean", "description": "Force re-download even if cache exists", "default": false }
                },
                "required": []
            }),
            |args, ctx| async move { handle_download_jlcpcb(args, ctx).await }
        ),
        tool!(
            "search_jlcpcb_parts",
            "Search the local JLCPCB component database by keyword, value, or category. The query is split on whitespace and a part matches only if every word appears, in any order, in its LCSC number, MPN, package, manufacturer or description; case-insensitive. A word that is a value (a number with an optional prefix and unit, like 20pF, 4.7uF, 1k or 0402) must match as a whole value: 20pF does not find 220pF. Unit spellings are not converted: 100nF does not find 0.1uF. Results rank in-stock parts first, then as suggest_jlcpcb_alternatives ranks them: Basic, then Preferred, then Extended parts, then known price before unknown, price ascending, stock descending, LCSC number.",
            json!({
                "type": "object",
                "properties": {
                    "query": { "type": "string", "description": "Search words, separated by spaces. A part must contain every word, in any order, in its LCSC number, MPN, package, manufacturer or description (case-insensitive; % and _ are literal). Value words such as 20pF or 1k match only as whole values" },
                    "category": { "type": "string", "description": "Only parts whose category contains this text (case-insensitive; % and _ are literal). Categories read 'Group / Subcategory', e.g. 'Optoelectronics / LED Indication - Discrete'; list them with list_jlcpcb_categories" },
                    "basic_only": { "type": "boolean", "description": "Restrict to JLCPCB Basic Library parts only", "default": false },
                    "in_stock": { "type": "boolean", "description": "Only return parts known to be in stock. Parts whose stock the catalog does not know are left out and counted in unknown_stock_excluded", "default": true },
                    "limit": { "type": "integer", "description": "Maximum number of results", "default": 20 }
                },
                "required": ["query"]
            }),
            |args, ctx| async move { handle_search_jlcpcb_parts(args, ctx).await }
        ),
        tool!(
            "list_jlcpcb_categories",
            "List the part categories in the local JLCPCB database, for search_jlcpcb_parts' category filter. Without contains, lists the top-level groups (e.g. 'Optoelectronics') with their part and subcategory counts. With contains, lists every full category name ('Group / Subcategory') containing that text, with its part counts.",
            json!({
                "type": "object",
                "properties": {
                    "contains": { "type": "string", "description": "Only full category names containing this text, case-insensitive (optional). Without it, top-level groups are listed" }
                },
                "required": []
            }),
            |args, ctx| async move { handle_list_jlcpcb_categories(args, ctx).await }
        ),
        tool!(
            "get_jlcpcb_part",
            "Retrieve full details for a single JLCPCB part by its LCSC part number.",
            json!({
                "type": "object",
                "properties": {
                    "lcsc_id": { "type": "string", "description": "LCSC part number (e.g. 'C14663')" }
                },
                "required": ["lcsc_id"]
            }),
            |args, ctx| async move { handle_get_jlcpcb_part(args, ctx).await }
        ),
        tool!(
            "import_jlcpcb_part",
            "Import a symbol and footprint directly from JLCPCB / EasyEDA by LCSC part number (e.g. 'C2856837'). Converts pins, pads, graphics, and custom properties (LCSC, MPN, Manufacturer, JLCPCB_Class, verified datasheet URL), saves to the project's KiCad symbol and footprint libraries, idempotently registers both in sym-lib-table and fp-lib-table, and optionally places the component onto the schematic.",
            json!({
                "type": "object",
                "properties": {
                    "project_dir": { "type": "string", "description": "Path to KiCad project directory (where .kicad_pro, sym-lib-table, and fp-lib-table live). Uses server config default if omitted" },
                    "lcsc_id": { "type": "string", "description": "LCSC part number (e.g. 'C2856837')" },
                    "library_name": { "type": "string", "description": "Library nickname and filename to store symbol and footprint (default: 'jlcpcb')", "default": "jlcpcb" },
                    "include_3d": { "type": "boolean", "description": "Whether to attempt downloading 3D STEP/OBJ model if available", "default": false },
                    "schematic": { "type": "string", "description": "Path to .kicad_sch to place component immediately after importing (optional)" },
                    "x": { "type": "number", "description": "X coordinate in mm on schematic if placing" },
                    "y": { "type": "number", "description": "Y coordinate in mm on schematic if placing" },
                    "reference": { "type": "string", "description": "Optional reference designator override if placing" },
                    "value": { "type": "string", "description": "Optional value override if placing" }
                },
                "required": ["lcsc_id"]
            }),
            |args, ctx| async move { super::easyeda::handle_import_jlcpcb_part(args, ctx).await }
        ),
        tool!(
            "suggest_jlcpcb_alternatives",
            "Suggest JLCPCB-stocked parts with the same value in the package a KiCad footprint uses. The footprint is mapped to LCSC's package names, the value must match as a whole value, and parts below a stock floor are excluded. Results rank Basic, then Preferred, then Extended parts, then known price and stock; the response states the package names matched, the ranking, and what was excluded.",
            json!({
                "type": "object",
                "properties": {
                    "value": { "type": "string", "description": "Component value or part number, matched case-insensitively as a whole value (no digit or '.' directly before or after) against the part description and manufacturer part number, e.g. '100nF', '10k', 'AMS1117-3.3'. Unit spellings are not converted: '100nF' does not find '0.1uF'" },
                    "footprint": { "type": "string", "description": "KiCad footprint ID such as 'Capacitor_SMD:C_0402_1005Metric', or LCSC's own package name without a library prefix, such as '0402' or 'LQFP-48(7x7)'. A library footprint with no known LCSC package is refused" },
                    "max_price_usd": { "type": "number", "description": "Keep only parts whose known unit price is at or below this, in USD (optional). Parts with an unknown price are excluded when it is set" },
                    "min_stock_count": { "type": "integer", "minimum": 0, "description": "Exclude parts with fewer units in stock. Raise it to the build quantity", "default": 100 },
                    "prefer_basic": { "type": "boolean", "description": "Rank JLCPCB Basic parts first, then Preferred, then Extended, before price. Extended parts add a setup fee per unique part", "default": true },
                    "limit": { "type": "integer", "description": "Maximum number of suggestions", "default": 5 }
                },
                "required": ["value", "footprint"]
            }),
            |args, ctx| async move { handle_suggest_alternatives(args, ctx).await }
        ),
        tool!(
            "get_jlcpcb_database_stats",
            "Return statistics about the local JLCPCB database cache: part count, category count, file size.",
            json!({
                "type": "object",
                "properties": {},
                "required": []
            }),
            |args, ctx| async move { handle_jlcpcb_stats(args, ctx).await }
        ),
        tool!(
            "enrich_datasheets",
            "Fetch and cache datasheet URLs for all components in a schematic using the local JLCPCB catalog with LCSC API fallback.",
            json!({
                "type": "object",
                "properties": {
                    "schematic": { "type": "string", "description": "Path to .kicad_sch file" },
                    "overwrite_existing": { "type": "boolean", "description": "Replace existing Datasheet fields", "default": false },
                    "db_path": { "type": "string", "description": "Local path to the JLCPCB SQLite database (optional, uses config default)" }
                },
                "required": ["schematic"]
            }),
            |args, ctx| async move { handle_enrich_datasheets(args, ctx).await }
        ),
        tool!(
            "get_datasheet_url",
            "Retrieve the datasheet URL for a component by MPN or LCSC ID.",
            json!({
                "type": "object",
                "properties": {
                    "mpn": { "type": "string", "description": "Manufacturer part number (optional)" },
                    "lcsc_id": { "type": "string", "description": "LCSC part number (optional)" }
                },
                "required": []
            }),
            |args, ctx| async move { handle_get_datasheet_url(args, ctx).await }
        ),
        tool!(
            "check_freerouting",
            "Locate a Freerouting installation, including KiCad PCM plugin directories, and verify that its Java runtime is available.",
            json!({
                "type": "object",
                "properties": {
                    "jar_path": { "type": "string", "description": freerouting_jar_path_description() }
                },
                "required": []
            }),
            |args, ctx| async move { handle_check_freerouting(args, ctx).await }
        ),
        tool!(
            "route_specctra_dsn",
            "Route an existing Specctra DSN through the local Freerouting JAR's native headless MCP server and create a new SES file. Uses the documented session/job state machine, never sends board data to a cloud service, and never replaces an existing output.",
            json!({
                "type": "object",
                "properties": {
                    "dsn_path": { "type": "string", "description": "Existing Specctra .dsn input" },
                    "ses_output_path": { "type": "string", "description": "New .ses output path; existing files are never replaced" },
                    "jar_path": { "type": "string", "description": freerouting_jar_path_description() },
                    "max_passes": { "type": "integer", "minimum": 1, "maximum": 100 },
                    "optimizer_enabled": { "type": "boolean" },
                    "job_timeout_seconds": { "type": "integer", "minimum": 1, "maximum": 86400 },
                    "overall_timeout_seconds": { "type": "integer", "minimum": 10, "maximum": 86400, "default": 900 }
                },
                "required": ["dsn_path", "ses_output_path"]
            }),
            |args, ctx| async move { handle_route_specctra_dsn(args, ctx).await }
        ),
    ]
}

// ─── JLCPCB database path helper ─────────────────────────────────────────────

/// [`resolve_db_path`], or the refusal to return when there is none.
fn require_db_path(args: &serde_json::Value, ctx: &ToolContext) -> Result<PathBuf, CallToolResult> {
    super::require_user_path(
        resolve_db_path(args, ctx),
        "the JLCPCB database (set jlcpcb_db_path in the server config)",
    )
}

#[cfg(test)]
mod configured_db_path_tests {
    use super::*;
    use crate::mcp::handler::McpHandler;
    use crate::tools::ServerConfig;

    #[tokio::test]
    async fn blank_database_configuration_uses_the_default_through_served_tools() {
        const CHILD: &str = "COPPERPOUR_TEST_JLCPCB_PATH_CHILD";
        if std::env::var_os(CHILD).is_none() {
            // Isolate platform path environment from other tests and user caches.
            let dir = tempfile::tempdir().unwrap();
            let output = std::process::Command::new(std::env::current_exe().unwrap())
                .args([
                    "--exact",
                    "tools::integration::configured_db_path_tests::blank_database_configuration_uses_the_default_through_served_tools",
                    "--nocapture",
                ])
                .env(CHILD, "1")
                .env("APPDATA", dir.path())
                .env("HOME", dir.path())
                .output()
                .unwrap();
            assert!(
                output.status.success(),
                "{}\n{}",
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            );
            return;
        }

        let default = default_jlcpcb_db_path().unwrap();
        std::fs::create_dir_all(default.parent().unwrap()).unwrap();
        let configured = default.with_file_name(" configured.db");
        let override_path = default.with_file_name("override.db");
        for path in [&default, &configured, &override_path] {
            let conn = rusqlite::Connection::open(path).unwrap();
            conn.execute_batch("CREATE TABLE components (LCSC TEXT, MFR_Part TEXT, Package TEXT, Manufacturer TEXT, Library_Type TEXT, Description TEXT, Datasheet TEXT, Price REAL, Stock INTEGER, Category TEXT);")
                .unwrap();
            conn.execute("INSERT INTO components VALUES ('C678', ?1, '0603', 'Test', 'Basic', 'path probe', '', 0.1, 10, 'Test')", [path.to_str().unwrap()]).unwrap();
        }
        for (setting, expected) in [
            (None, default.clone()),
            (Some(PathBuf::new()), default.clone()),
            (Some(PathBuf::from(" \t\n ")), default.clone()),
            (Some(configured.clone()), configured.clone()),
        ] {
            let config = ServerConfig {
                jlcpcb_db_path: setting.clone(),
                eager_toolsets: true,
                ..Default::default()
            };
            let direct = ToolContext::new(
                config.clone(),
                std::sync::Arc::new(crate::router::ToolRouter::new()),
            );
            assert_eq!(resolve_db_path(&json!({}), &direct), Some(expected.clone()));
            let handler = McpHandler::new(config).await.unwrap();
            for selected in [&expected, &override_path] {
                for (name, mut arguments) in [
                    ("download_jlcpcb_database", json!({})),
                    ("search_jlcpcb_parts", json!({"query": "path probe"})),
                ] {
                    if selected == &override_path {
                        if name == "search_jlcpcb_parts" {
                            continue;
                        }
                        arguments["output_path"] = json!(override_path);
                    }
                    let response = handler
                        .handle_message(json!({
                            "jsonrpc": "2.0", "id": 678, "method": "tools/call",
                            "params": {"name": name, "arguments": arguments}
                        }))
                        .await
                        .unwrap()
                        .result
                        .unwrap();
                    assert_ne!(response["isError"], true, "{name}: {response}");
                    let body: serde_json::Value =
                        serde_json::from_str(response["content"][0]["text"].as_str().unwrap())
                            .unwrap();
                    if name == "download_jlcpcb_database" {
                        assert_eq!(body["status"], "already_exists");
                        assert_eq!(body["path"], json!(selected));
                    } else {
                        assert_eq!(body["results"][0]["mpn"], json!(selected));
                    }
                }
            }
        }
    }
}

// ─── Handlers ─────────────────────────────────────────────────────────────────

async fn handle_download_jlcpcb(
    args: &serde_json::Value,
    ctx: &ToolContext,
) -> anyhow::Result<CallToolResult> {
    let db_path = match require_db_path(args, ctx) {
        Ok(path) => path,
        Err(refusal) => return Ok(refusal),
    };
    let force = args["force"].as_bool().unwrap_or(false);

    if db_path.exists() && !force {
        let meta = tokio::fs::metadata(&db_path).await?;
        return Ok(CallToolResult::text(
            serde_json::to_string(&json!({
                "status": "already_exists",
                "path": db_path.to_str().unwrap_or(""),
                "size_bytes": meta.len(),
                "note": "Use force=true to re-download"
            }))
            .unwrap(),
        ));
    }

    if let Some(parent) = db_path.parent() {
        tokio::fs::create_dir_all(parent).await?;
    }

    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(300))
        .build()?;

    let db_parent = db_path.parent().unwrap_or_else(|| Path::new("."));
    let work_dir = tempfile::Builder::new()
        .prefix("copperpour-jlcpcb-")
        .tempdir_in(db_parent)?;
    let archive_path = work_dir.path().join(JLCPCB_FEED_ARCHIVE);
    let upstream_path = work_dir.path().join("upstream.db");
    let staged_path = work_dir.path().join("konnect.db");
    let backup_path = work_dir.path().join("previous.db");

    let (chunk_count, downloaded_bytes) = download_jlcpcb_archive(&client, &archive_path).await?;
    let (part_count, size_bytes) = tokio::task::spawn_blocking({
        let db_path = db_path.clone();
        move || -> anyhow::Result<(u64, u64)> {
            extract_jlcpcb_database(&archive_path, &upstream_path)?;
            let part_count = build_konnect_jlcpcb_database(&upstream_path, &staged_path)?;
            let size_bytes = std::fs::metadata(&staged_path)?.len();
            replace_jlcpcb_database(&staged_path, &db_path, &backup_path)?;
            Ok((part_count, size_bytes))
        }
    })
    .await??;

    Ok(CallToolResult::text(
        serde_json::to_string(&json!({
            "success": true,
            "path": db_path.to_str().unwrap_or(""),
            "size_bytes": size_bytes,
            "part_count": part_count,
            "source": JLCPCB_FEED_BASE_URL,
            "downloaded_chunks": chunk_count,
            "downloaded_bytes": downloaded_bytes
        }))
        .unwrap(),
    ))
}

/// Build a deterministic cache key from a tool name, the resolved DB path
/// (so pointing at a different `output_path` never serves stale results),
/// and the query parameters that affect the result set.
fn cache_key(tool: &str, db_path: &std::path::Path, parts: &[&str]) -> String {
    format!("{}|{}|{}", tool, db_path.display(), parts.join("|"))
}

async fn handle_search_jlcpcb_parts(
    args: &serde_json::Value,
    ctx: &ToolContext,
) -> anyhow::Result<CallToolResult> {
    // Arguments before environment: a missing `query` is the caller's
    // mistake and is theirs to fix, whereas a missing database is transient
    // and unrelated. Checking the database first told someone who had simply
    // forgotten the query to go and download a 2.5M-part catalogue (#218).
    let query = match require_str(args, "query") {
        Ok(v) => v.to_string(),
        Err(e) => return Ok(e),
    };

    let db_path = match require_db_path(args, ctx) {
        Ok(path) => path,
        Err(refusal) => return Ok(refusal),
    };
    if !db_path.exists() {
        return Ok(CallToolResult::error(
            "JLCPCB database not found. Run download_jlcpcb_database first.",
        ));
    }
    let basic_only = args["basic_only"].as_bool().unwrap_or(false);
    let in_stock = args["in_stock"].as_bool().unwrap_or(true);
    let limit = args["limit"].as_u64().unwrap_or(20) as usize;
    let category = args["category"].as_str().map(String::from);

    let key = cache_key(
        "search_jlcpcb_parts",
        &db_path,
        &[
            &query,
            category.as_deref().unwrap_or(""),
            &basic_only.to_string(),
            &in_stock.to_string(),
            &limit.to_string(),
        ],
    );
    if let Some(cached) = ctx.jlcpcb_cache.get(&key) {
        let mut body = cached;
        body["cached"] = json!(true);
        return Ok(CallToolResult::text(serde_json::to_string(&body).unwrap()));
    }

    let tokens = jlcpcb_search_tokens(&query);
    let sql_tokens = tokens.clone();
    let query_for_fallback = query.clone();
    // The parts found, how many parts with unknown stock `in_stock` left out
    // (counted only where the catalog can say so), and the catalog's version.
    type SearchOutcome = (Vec<serde_json::Value>, Option<usize>, i64);
    let (results, unknown_stock_excluded, schema_version) = tokio::task::spawn_blocking(move || -> anyhow::Result<SearchOutcome> {
        let conn = rusqlite::Connection::open(&db_path)?;
        let schema_version = catalog_schema_version(&conn)?;
        // A catalog from before #63 stored unknown stock as 0, so it has none
        // to count.
        let count_unknown_stock = in_stock && schema_version >= JLCPCB_CATALOG_SCHEMA_VERSION;

        // The JLCPCB db schema has columns: LCSC, MFR_Part, Package, Solder_Joint,
        // Manufacturer, Library_Type, Description, Datasheet, Price, Stock
        //
        // Every whitespace-separated word must match somewhere in one of
        // the searchable columns, in any order (#432). `%`, `_` and `\` in a
        // word are matched literally, and every value is a bound parameter.
        let mut sql = String::from(
            "SELECT LCSC, MFR_Part, Package, Manufacturer, Library_Type, Description, Datasheet, Price, Stock \
             FROM components WHERE 1=1",
        );
        let mut params: Vec<String> = Vec::new();
        for token in &sql_tokens {
            params.push(format!("%{}%", escape_like(token)));
            let n = params.len();
            sql.push_str(&format!(
                " AND (Description LIKE ?{n} ESCAPE '\\' OR MFR_Part LIKE ?{n} ESCAPE '\\' \
                 OR LCSC LIKE ?{n} ESCAPE '\\' OR Package LIKE ?{n} ESCAPE '\\' \
                 OR Manufacturer LIKE ?{n} ESCAPE '\\')"
            ));
        }
        if basic_only {
            sql.push_str(" AND Library_Type = 'Basic'");
        }
        if let Some(ref cat) = category {
            params.push(format!("%{}%", escape_like(cat)));
            sql.push_str(&format!(" AND Category LIKE ?{} ESCAPE '\\'", params.len()));
        }
        // `in_stock` keeps parts whose stock is known and above 0; those with
        // unknown stock are counted instead of silently dropped (#63).
        let unknown_stock_sql = format!("{sql} AND Stock IS NULL");
        if in_stock {
            sql.push_str(JLCPCB_IN_STOCK_FILTER);
        }
        // Ranked in-stock first, then as suggest_jlcpcb_alternatives ranks
        // (#90), ending on the LCSC number so a page is the same on every run
        // (#432). The LIKE
        // above cannot see value boundaries, so value words are checked
        // here, and the limit is applied after that check.
        sql.push_str(&jlcpcb_ranking_order_by());

        let value_tokens: Vec<&String> = sql_tokens
            .iter()
            .filter(|token| is_jlcpcb_value_token(token))
            .collect();
        let unknown_stock = |sql: &str, params: &[String], value_tokens: &[&String]| {
            count_unknown_stock
                .then(|| {
                    let mut statement = conn.prepare(sql)?;
                    collect_catalog_parts(&mut statement, params, value_tokens, usize::MAX)
                        .map(|parts| parts.len())
                })
                .transpose()
        };
        let mut stmt = conn.prepare(&sql)?;
        let rows = collect_catalog_parts(&mut stmt, &params, &value_tokens, limit)?;
        let main_unknown_stock = unknown_stock(&unknown_stock_sql, &params, &value_tokens)?;

        if !rows.is_empty() {
            return Ok((rows, main_unknown_stock, schema_version));
        }

        // Fallback: If exact token search yielded 0 results, retry with punctuation-normalized
        // MPN and token matching. This resolves part numbers where hyphens or underscores
        // were omitted, altered, or where a URL slug combines manufacturer and part number
        // (e.g. "XUNPU-FPC_05FB40PH20" or "STM32F103C8T6").
        let fallback_tokens: Vec<String> = query_for_fallback
            .split(|c: char| c.is_whitespace() || c == '-' || c == '_' || c == '/')
            .filter(|s| !s.is_empty())
            .map(String::from)
            .collect();

        if fallback_tokens.is_empty() {
            return Ok((Vec::new(), main_unknown_stock, schema_version));
        }

        let mut fallback_sql = String::from(
            "SELECT LCSC, MFR_Part, Package, Manufacturer, Library_Type, Description, Datasheet, Price, Stock \
             FROM components WHERE 1=1",
        );
        let mut fallback_params: Vec<String> = Vec::new();
        for token in &fallback_tokens {
            fallback_params.push(format!("%{}%", escape_like(token)));
            let n1 = fallback_params.len();

            let clean: String = token.chars().filter(|c| c.is_ascii_alphanumeric()).collect();
            if clean.len() >= 2 {
                fallback_params.push(format!("%{}%", escape_like(&clean)));
                let n2 = fallback_params.len();
                fallback_sql.push_str(&format!(
                    " AND (Description LIKE ?{n1} ESCAPE '\\' OR MFR_Part LIKE ?{n1} ESCAPE '\\' \
                     OR LCSC LIKE ?{n1} ESCAPE '\\' OR Package LIKE ?{n1} ESCAPE '\\' \
                     OR Manufacturer LIKE ?{n1} ESCAPE '\\' \
                     OR REPLACE(REPLACE(REPLACE(REPLACE(MFR_Part, '-', ''), '_', ''), '/', ''), ' ', '') LIKE ?{n2} ESCAPE '\\' \
                     OR REPLACE(REPLACE(REPLACE(REPLACE(COALESCE(Manufacturer, '') || MFR_Part, '-', ''), '_', ''), '/', ''), ' ', '') LIKE ?{n2} ESCAPE '\\')"
                ));
            } else {
                fallback_sql.push_str(&format!(
                    " AND (Description LIKE ?{n1} ESCAPE '\\' OR MFR_Part LIKE ?{n1} ESCAPE '\\' \
                     OR LCSC LIKE ?{n1} ESCAPE '\\' OR Package LIKE ?{n1} ESCAPE '\\' \
                     OR Manufacturer LIKE ?{n1} ESCAPE '\\')"
                ));
            }
        }
        if basic_only {
            fallback_sql.push_str(" AND Library_Type = 'Basic'");
        }
        if let Some(ref cat) = category {
            fallback_params.push(format!("%{}%", escape_like(cat)));
            fallback_sql.push_str(&format!(
                " AND Category LIKE ?{} ESCAPE '\\'",
                fallback_params.len()
            ));
        }
        let fallback_unknown_stock_sql = format!("{fallback_sql} AND Stock IS NULL");
        if in_stock {
            fallback_sql.push_str(JLCPCB_IN_STOCK_FILTER);
        }
        fallback_sql.push_str(&jlcpcb_ranking_order_by());

        let fallback_value_tokens: Vec<&String> = fallback_tokens
            .iter()
            .filter(|token| is_jlcpcb_value_token(token))
            .collect();
        let mut fallback_stmt = conn.prepare(&fallback_sql)?;
        let fallback_rows =
            collect_catalog_parts(&mut fallback_stmt, &fallback_params, &fallback_value_tokens, limit)?;
        // Count the unknown-stock parts of the query whose results are
        // returned: the fallback's when it found some, else the main query's.
        let fallback_unknown_stock = if fallback_rows.is_empty() {
            main_unknown_stock
        } else {
            unknown_stock(&fallback_unknown_stock_sql, &fallback_params, &fallback_value_tokens)?
        };

        Ok((fallback_rows, fallback_unknown_stock, schema_version))
    })
    .await??;

    let mut body = json!({
        "query": args["query"].as_str().unwrap_or(""),
        "count": results.len(),
        "results": results,
        "tokens": tokens,
        "value_tokens": tokens.iter().filter(|t| is_jlcpcb_value_token(t)).collect::<Vec<_>>(),
        "search_fields": JLCPCB_SEARCH_FIELDS,
        "ranking": search_ranking_criteria()
    });
    if let Some(excluded) = unknown_stock_excluded {
        body["unknown_stock_excluded"] = json!(excluded);
    }
    if let Some(note) = old_catalog_note(schema_version) {
        body["catalog_note"] = json!(note);
    }
    ctx.jlcpcb_cache.put(key, body.clone());

    body["cached"] = json!(false);
    Ok(CallToolResult::text(serde_json::to_string(&body).unwrap()))
}

async fn handle_list_jlcpcb_categories(
    args: &serde_json::Value,
    ctx: &ToolContext,
) -> anyhow::Result<CallToolResult> {
    let db_path = match require_db_path(args, ctx) {
        Ok(path) => path,
        Err(refusal) => return Ok(refusal),
    };
    if !db_path.exists() {
        return Ok(CallToolResult::error(
            "JLCPCB database not found. Run download_jlcpcb_database first.",
        ));
    }
    let contains = args["contains"]
        .as_str()
        .map(str::trim)
        .filter(|text| !text.is_empty())
        .map(str::to_lowercase);

    let rows = tokio::task::spawn_blocking(move || -> anyhow::Result<Vec<(String, i64, i64)>> {
        let conn = rusqlite::Connection::open_with_flags(
            &db_path,
            rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
        )?;
        let mut stmt = conn.prepare(
            "SELECT Category, COUNT(*), SUM(Stock > 0) FROM components \
             WHERE COALESCE(Category, '') <> '' GROUP BY Category ORDER BY Category",
        )?;
        let rows = stmt
            .query_map([], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)))?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    })
    .await??;

    let body = match contains {
        Some(text) => {
            let categories: Vec<serde_json::Value> = rows
                .into_iter()
                .filter(|(category, _, _)| category.to_lowercase().contains(&text))
                .map(|(category, parts, in_stock)| {
                    json!({
                        "category": category,
                        "part_count": parts,
                        "in_stock_count": in_stock
                    })
                })
                .collect();
            json!({
                "count": categories.len(),
                "categories": categories,
                "note": "Full category names. Pass one, or any part of one, as search_jlcpcb_parts' category."
            })
        }
        None => {
            // (parts, in stock, subcategories) per top-level group.
            let mut groups: std::collections::BTreeMap<String, (i64, i64, usize)> =
                std::collections::BTreeMap::new();
            for (category, parts, in_stock) in rows {
                let (group, subcategory) = match category.split_once(JLCPCB_CATEGORY_SEPARATOR) {
                    Some((group, _)) => (group.to_owned(), 1),
                    None => (category, 0),
                };
                let entry = groups.entry(group).or_default();
                entry.0 += parts;
                entry.1 += in_stock;
                entry.2 += subcategory;
            }
            let categories: Vec<serde_json::Value> = groups
                .into_iter()
                .map(|(group, (parts, in_stock, subcategories))| {
                    json!({
                        "category": group,
                        "part_count": parts,
                        "in_stock_count": in_stock,
                        "subcategory_count": subcategories
                    })
                })
                .collect();
            json!({
                "count": categories.len(),
                "categories": categories,
                "note": "Top-level groups. Pass contains, e.g. a group name, to list its full category names."
            })
        }
    };
    Ok(CallToolResult::text(serde_json::to_string(&body).unwrap()))
}

async fn handle_get_jlcpcb_part(
    args: &serde_json::Value,
    ctx: &ToolContext,
) -> anyhow::Result<CallToolResult> {
    let db_path = match require_db_path(args, ctx) {
        Ok(path) => path,
        Err(refusal) => return Ok(refusal),
    };
    if !db_path.exists() {
        return Ok(CallToolResult::error(
            "JLCPCB database not found. Run download_jlcpcb_database first.",
        ));
    }
    let lcsc_id = require_str(args, "lcsc_id")
        .map_err(|e| anyhow::anyhow!("{:?}", e))?
        .to_string();

    let key = cache_key("get_jlcpcb_part", &db_path, &[&lcsc_id]);
    if let Some(mut cached) = ctx.jlcpcb_cache.get(&key) {
        cached["cached"] = json!(true);
        return Ok(CallToolResult::text(
            serde_json::to_string(&cached).unwrap(),
        ));
    }

    let result =
        tokio::task::spawn_blocking(move || -> anyhow::Result<Option<serde_json::Value>> {
            let conn = rusqlite::Connection::open(&db_path)?;
            let schema_version = catalog_schema_version(&conn)?;
            let mut stmt = conn.prepare(
            "SELECT LCSC, MFR_Part, Package, Manufacturer, Library_Type, Description, Datasheet, Price, Stock \
             FROM components WHERE LCSC = ?1 LIMIT 1"
        )?;
            let mut rows = stmt.query_map(rusqlite::params![lcsc_id], row_to_part_json)?;
            // A row that cannot be read is an error, not "not found" (#63).
            let part = rows.next().transpose().map_err(|error| {
                anyhow::anyhow!("cannot read part {lcsc_id} from the JLCPCB catalog: {error}")
            })?;
            Ok(part.map(|mut part| {
                if let Some(note) = old_catalog_note(schema_version) {
                    part["catalog_note"] = json!(note);
                }
                part
            }))
        })
        .await??;

    match result {
        Some(part) => {
            ctx.jlcpcb_cache.put(key, part.clone());
            let mut part = part;
            part["cached"] = json!(false);
            Ok(CallToolResult::text(serde_json::to_string(&part).unwrap()))
        }
        None => Ok(CallToolResult::error(format!(
            "Part not found in database: {}",
            args["lcsc_id"].as_str().unwrap_or("")
        ))),
    }
}

async fn handle_suggest_alternatives(
    args: &serde_json::Value,
    ctx: &ToolContext,
) -> anyhow::Result<CallToolResult> {
    // Arguments before environment — see `handle_search_jlcpcb_parts`.
    let value = match require_str(args, "value") {
        Ok(v) => v.trim().to_string(),
        Err(e) => return Ok(e),
    };
    let footprint = match require_str(args, "footprint") {
        Ok(v) => v.trim().to_string(),
        Err(e) => return Ok(e),
    };
    if value.is_empty() {
        return Ok(CallToolResult::error_kind(
            ToolErrorKind::InvalidArgument {
                field: "value".into(),
                reason: "empty".into(),
            },
            "value is empty; give the component value or part number to match",
        ));
    }
    // The package used to be the footprint ID's last `_` segment, which for
    // every standard KiCad footprint (`C_0402_1005Metric` → `1005Metric`) is
    // a name LCSC never uses, so the search silently matched nothing (#785).
    let Some(package_match) = lcsc_packages::lcsc_packages(&footprint) else {
        return Ok(CallToolResult::error_kind(
            ToolErrorKind::InvalidArgument {
                field: "footprint".into(),
                reason: "no known LCSC package for this footprint".into(),
            },
            format!(
                "No LCSC package name is known for footprint '{footprint}'. Pass LCSC's own \
                 package name without a library prefix instead, for example '0402', 'SOT-23' \
                 or 'LQFP-48(7x7)'."
            ),
        ));
    };

    let db_path = match require_db_path(args, ctx) {
        Ok(path) => path,
        Err(refusal) => return Ok(refusal),
    };
    if !db_path.exists() {
        return Ok(CallToolResult::error(
            "JLCPCB database not found. Run download_jlcpcb_database first.",
        ));
    }
    let policy = AlternativePolicy {
        min_stock_count: args["min_stock_count"]
            .as_u64()
            .unwrap_or(SUGGEST_DEFAULT_MIN_STOCK_COUNT),
        prefer_basic: args["prefer_basic"].as_bool().unwrap_or(true),
        max_price: args["max_price_usd"].as_f64(),
        limit: args["limit"].as_u64().unwrap_or(5) as usize,
    };

    let key = cache_key(
        "suggest_jlcpcb_alternatives",
        &db_path,
        &[
            &value,
            &footprint,
            &policy.max_price.map(|v| v.to_string()).unwrap_or_default(),
            &policy.limit.to_string(),
            &policy.min_stock_count.to_string(),
            &policy.prefer_basic.to_string(),
        ],
    );
    if let Some(cached) = ctx.jlcpcb_cache.get(&key) {
        let mut body = cached;
        body["cached"] = json!(true);
        return Ok(CallToolResult::text(serde_json::to_string(&body).unwrap()));
    }

    let packages = package_match.packages.clone();
    let query_value = value.clone();
    let (candidates, schema_version) = tokio::task::spawn_blocking(move || {
        query_alternative_candidates(&db_path, &query_value, &packages)
    })
    .await??;
    let (alternatives, counts) = rank_alternatives(candidates, &policy);

    let body = json!({
        "value": value,
        "footprint": footprint,
        "package_match": {
            "rule": package_match.rule.name(),
            "lcsc_packages": package_match.packages,
            "note": "Matched by LCSC's package name, which does not record pitch or exposed-pad size; check the land pattern against the datasheet."
        },
        "value_match": {
            "columns": ["Description", "MFR_Part"],
            "rule": "whole_value",
            "note": "Case-insensitive, with no digit or '.' directly before or after the value. Unit spellings are not converted: '100nF' does not find '0.1uF'."
        },
        "ranking": {
            "criteria": policy.criteria(),
            "prefer_basic": policy.prefer_basic,
            "min_stock_count": policy.min_stock_count,
            "max_price_usd": policy.max_price
        },
        "count": counts.returned,
        "matched_count": counts.matched,
        "exclusions": {
            "unknown_stock_count": counts.unknown_stock,
            "below_min_stock_count": counts.below_min_stock,
            "unknown_price_count": counts.unknown_price,
            "above_max_price_count": counts.above_max_price
        },
        "alternatives": alternatives.into_iter().map(|candidate| candidate.part).collect::<Vec<_>>()
    });
    let mut body = body;
    if let Some(note) = old_catalog_note(schema_version) {
        body["catalog_note"] = json!(note);
    }
    ctx.jlcpcb_cache.put(key, body.clone());

    body["cached"] = json!(false);
    Ok(CallToolResult::text(serde_json::to_string(&body).unwrap()))
}

async fn handle_jlcpcb_stats(
    args: &serde_json::Value,
    ctx: &ToolContext,
) -> anyhow::Result<CallToolResult> {
    let db_path = match require_db_path(args, ctx) {
        Ok(path) => path,
        Err(refusal) => return Ok(refusal),
    };
    if !db_path.exists() {
        return Ok(CallToolResult::text(
            serde_json::to_string(&json!({
                "exists": false,
                "note": "Run download_jlcpcb_database to fetch the parts database"
            }))
            .unwrap(),
        ));
    }

    let meta = tokio::fs::metadata(&db_path).await?;
    let size_bytes = meta.len();

    // How many prices and stock counts are unknown, where the catalog can say.
    type Stats = (i64, i64, i64, Option<(i64, i64)>);
    let (count, category_count, schema_version, unknown) = tokio::task::spawn_blocking({
        let db_path = db_path.clone();
        move || -> anyhow::Result<Stats> {
            let conn = rusqlite::Connection::open(&db_path)?;
            let count: i64 = conn.query_row("SELECT COUNT(*) FROM components", [], |r| r.get(0))?;
            let category_count: i64 = conn.query_row(
                "SELECT COUNT(DISTINCT Category) FROM components WHERE COALESCE(Category, '') <> ''",
                [],
                |r| r.get(0),
            )?;
            let schema_version = catalog_schema_version(&conn)?;
            let unknown = if schema_version >= JLCPCB_CATALOG_SCHEMA_VERSION {
                Some(conn.query_row(
                    "SELECT SUM(Price IS NULL), SUM(Stock IS NULL) FROM components",
                    [],
                    |r| Ok((r.get::<_, Option<i64>>(0)?.unwrap_or(0), r.get::<_, Option<i64>>(1)?.unwrap_or(0))),
                )?)
            } else {
                None
            };
            Ok((count, category_count, schema_version, unknown))
        }
    })
    .await??;

    let mut body = json!({
        "exists": true,
        "path": db_path.to_str().unwrap_or(""),
        "size_bytes": size_bytes,
        "part_count": count,
        "category_count": category_count,
        "schema_version": schema_version
    });
    if let Some((unknown_price, unknown_stock)) = unknown {
        body["unknown_price_count"] = json!(unknown_price);
        body["unknown_stock_count"] = json!(unknown_stock);
    }
    if let Some(note) = old_catalog_note(schema_version) {
        body["catalog_note"] = json!(note);
    }
    Ok(CallToolResult::text(serde_json::to_string(&body).unwrap()))
}

/// What the local JLCPCB catalog contributed to a datasheet lookup.
enum CatalogDatasheets {
    /// The catalog was opened and queried.
    Read(CatalogRead),
    /// No catalog was configured, and none is at the default path.
    Skipped(String),
    /// The configured or default catalog exists but could not be read.
    Failed(String),
}

/// Look up datasheets in the local catalog. A `db_path` the caller gives must
/// be a readable catalog: a missing or unreadable one is refused as
/// `file_not_found` rather than replaced by the network.
async fn datasheets_from_catalog_for_lcsc_ids(
    args: &serde_json::Value,
    ctx: &ToolContext,
    lcsc_ids: &[String],
) -> Result<CatalogDatasheets, CallToolResult> {
    let explicit = args["db_path"].as_str().is_some();
    let Some(db_path) = resolve_db_path(args, ctx) else {
        return Ok(CatalogDatasheets::Skipped(
            "no catalog path: jlcpcb_db_path is not set and the home directory is unknown"
                .to_owned(),
        ));
    };
    let refuse = |reason: String| {
        CallToolResult::error_kind(
            ToolErrorKind::FileNotFound {
                path: db_path.display().to_string(),
            },
            reason,
        )
    };
    if !db_path.is_file() {
        if explicit {
            return Err(refuse(format!(
                "db_path is not a file: {}",
                db_path.display()
            )));
        }
        return Ok(CatalogDatasheets::Skipped(format!(
            "no catalog at {}",
            db_path.display()
        )));
    }
    let query_path = db_path.clone();
    let ids = lcsc_ids.to_vec();
    let queried = tokio::task::spawn_blocking(move || query_catalog_datasheets(&query_path, &ids))
        .await
        .map_err(anyhow::Error::from)
        .and_then(|result| result);
    match queried {
        Ok(read) => Ok(CatalogDatasheets::Read(read)),
        Err(error) => {
            let reason = format!("cannot read catalog {}: {error}", db_path.display());
            if explicit {
                Err(refuse(format!("db_path is unreadable: {reason}")))
            } else {
                Ok(CatalogDatasheets::Failed(reason))
            }
        }
    }
}

fn count_skipped_existing_values(
    symbols: &[DatasheetSymbolTarget],
    urls: &HashMap<String, String>,
    overwrite: bool,
) -> usize {
    if overwrite {
        return 0;
    }
    symbols
        .iter()
        .filter(|symbol| {
            urls.contains_key(&symbol.lcsc_id)
                && symbol
                    .datasheet
                    .as_deref()
                    .is_some_and(|d| !d.is_empty() && d != "~")
        })
        .count()
}

async fn handle_enrich_datasheets(
    args: &serde_json::Value,
    ctx: &ToolContext,
) -> anyhow::Result<CallToolResult> {
    let sch_path = get_path(args, "schematic")?;
    let overwrite = args["overwrite_existing"].as_bool().unwrap_or(false);

    let read_path = sch_path.clone();
    let content = tokio::task::spawn_blocking(move || read_consistent(&read_path)).await??;

    // Only placed, top-level symbol instances are enrichment targets. Library
    // definitions, quoted text, and nested decoys must not enter the target
    // set merely because they contain the same property spelling.
    let symbols = match validated_datasheet_symbols(&content) {
        Ok(symbols) => symbols,
        Err(error) => return Ok(error.into_result()),
    };
    let mut lcsc_ids: Vec<String> = symbols
        .iter()
        .map(|symbol| symbol.lcsc_id.clone())
        .collect();
    lcsc_ids.sort();
    lcsc_ids.dedup();

    if lcsc_ids.is_empty() {
        return Ok(CallToolResult::text(
            serde_json::to_string(&json!({
                "lcsc_ids_found": 0,
                "datasheets_enriched": 0,
                "local_hits": 0,
                "api_hits": 0,
                "misses": 0,
                "skipped_existing_values": 0,
                "attempted_sources": [],
                "updated": 0,
                "schematic": sch_path.to_str().unwrap_or(""),
                "note": "No LCSC property found in schematic components"
            }))
            .unwrap(),
        ));
    }

    let mut urls = HashMap::new();
    let mut attempted_sources = Vec::new();
    let mut local_catalog_skipped = None;
    let mut local_catalog_error = None;
    let mut local_catalog_unreadable_ids = Vec::new();

    let local_map = match datasheets_from_catalog_for_lcsc_ids(args, ctx, &lcsc_ids).await {
        Ok(CatalogDatasheets::Read(read)) => {
            attempted_sources.push("local_catalog".to_string());
            local_catalog_unreadable_ids = read.unreadable_ids;
            read.urls
        }
        Ok(CatalogDatasheets::Skipped(reason)) => {
            local_catalog_skipped = Some(reason);
            HashMap::new()
        }
        Ok(CatalogDatasheets::Failed(reason)) => {
            local_catalog_error = Some(reason);
            HashMap::new()
        }
        Err(refusal) => return Ok(refusal),
    };

    let mut local_hits = 0usize;
    let mut remaining_ids = Vec::new();

    for id in &lcsc_ids {
        if let Some(url) = local_map.get(id) {
            urls.insert(id.clone(), url.clone());
            local_hits += 1;
        } else {
            remaining_ids.push(id.clone());
        }
    }

    let mut api_hits = 0usize;
    let mut misses = 0usize;

    if !remaining_ids.is_empty() {
        attempted_sources.push("lcsc_api".to_string());
        let client = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(30))
            .build()?;

        for lcsc_id in &remaining_ids {
            let url = lcsc_detail_url(args, lcsc_id);
            let mut found = false;
            if let Ok(resp) = get_with_backoff(&client, &url).await {
                if resp.status().is_success() {
                    if let Ok(json_resp) = resp.json::<serde_json::Value>().await {
                        if let Some(datasheet_url) = json_resp
                            .pointer("/result/dataManualUrl")
                            .and_then(|v| v.as_str())
                            .map(str::trim)
                            .filter(|s| !s.is_empty() && *s != "~")
                        {
                            urls.insert(lcsc_id.clone(), datasheet_url.to_owned());
                            api_hits += 1;
                            found = true;
                        }
                    }
                }
            }
            if !found {
                misses += 1;
            }
        }
    }

    let skipped_existing = count_skipped_existing_values(&symbols, &urls, overwrite);

    let plan = match plan_datasheet_enrichment(&content, &urls, overwrite) {
        Ok(plan) => plan,
        Err(error) => return Ok(error.into_result()),
    };

    let observed_updates = if let Some((command, planned)) = plan {
        if let Err(error) = commit_command(&sch_path, &command) {
            if let Some(refusal) = datasheet_commit_refusal(&sch_path, &error) {
                return Ok(refusal);
            }
            return Err(error.into());
        }
        let committed = read_consistent(&sch_path)?;
        match observe_datasheet_updates(&committed, &planned) {
            Ok(observed) => observed,
            Err(error) => return Ok(error.into_result()),
        }
    } else {
        Vec::new()
    };

    let mut body = json!({
        "lcsc_ids_found": lcsc_ids.len(),
        "datasheets_enriched": observed_updates.len(),
        "local_hits": local_hits,
        "api_hits": api_hits,
        "misses": misses,
        "skipped_existing_values": skipped_existing,
        "attempted_sources": attempted_sources,
        "updated": observed_updates.len(),
        "schematic": sch_path.to_str().unwrap_or("")
    });
    if let Some(reason) = local_catalog_skipped {
        body["local_catalog_skipped"] = json!(reason);
        body["note"] = json!("No local JLCPCB catalog was read; run download_jlcpcb_database to resolve datasheets without the network");
    }
    if let Some(reason) = local_catalog_error {
        body["local_catalog_error"] = json!(reason);
        body["note"] = json!("The local JLCPCB catalog could not be read, so every part was looked up through the LCSC API; re-run download_jlcpcb_database with force to rebuild it");
    }
    if !local_catalog_unreadable_ids.is_empty() {
        body["local_catalog_unreadable_ids"] = json!(local_catalog_unreadable_ids);
        body["note"] = json!("The local JLCPCB catalog has a Datasheet that can't be read as text for the parts in local_catalog_unreadable_ids, so they were looked up through the LCSC API; re-run download_jlcpcb_database with force to rebuild it");
    }
    Ok(CallToolResult::text(serde_json::to_string(&body).unwrap()))
}

#[derive(Debug, Clone)]
struct DatasheetSymbolTarget {
    uuid: String,
    reference: String,
    unit: u32,
    lcsc_id: String,
    datasheet: Option<String>,
}

#[derive(Debug, Clone)]
struct PlannedDatasheetUpdate {
    target: DatasheetSymbolTarget,
    datasheet_url: String,
}

#[derive(Debug)]
enum DatasheetTargetError {
    Ambiguous {
        target: String,
        candidates: Vec<String>,
    },
    Stale {
        target: String,
        reason: String,
    },
}

impl DatasheetTargetError {
    fn stale(target: impl Into<String>, reason: impl Into<String>) -> Self {
        Self::Stale {
            target: target.into(),
            reason: reason.into(),
        }
    }

    fn from_sexp(target: impl Into<String>, error: SexpError) -> Self {
        Self::stale(target, error.to_string())
    }

    fn into_result(self) -> CallToolResult {
        match self {
            Self::Ambiguous { target, candidates } => {
                let reason = format!(
                    "more than one candidate was observed: {}",
                    candidates.join(", ")
                );
                CallToolResult::error_kind(
                    ToolErrorKind::StaleTarget {
                        target: target.clone(),
                        reason: reason.clone(),
                    },
                    format!("cannot safely enrich {target}: {reason}"),
                )
            }
            Self::Stale { target, reason } => CallToolResult::error_kind(
                ToolErrorKind::StaleTarget {
                    target: target.clone(),
                    reason: reason.clone(),
                },
                format!("cannot safely enrich {target}: {reason}"),
            ),
        }
    }
}

fn direct_property_values(node: &SexpNode, name: &str) -> Vec<String> {
    node.find_all("property")
        .into_iter()
        .filter(|property| property.get(1).and_then(SexpNode::as_str) == Some(name))
        .filter_map(|property| property.get(2).and_then(SexpNode::as_str))
        .map(ToOwned::to_owned)
        .collect()
}

fn one_identity_value(
    values: Vec<String>,
    field: &str,
    item: &str,
) -> Result<String, DatasheetTargetError> {
    match values.as_slice() {
        [value] if !value.trim().is_empty() => Ok(value.clone()),
        [] | [_] => Err(DatasheetTargetError::stale(
            item,
            format!("{field} is missing or empty"),
        )),
        _ => Err(DatasheetTargetError::Ambiguous {
            target: format!("{field} on {item}"),
            candidates: values,
        }),
    }
}

fn validated_datasheet_symbols(
    content: &str,
) -> Result<Vec<DatasheetSymbolTarget>, DatasheetTargetError> {
    let ranges = find_direct_child_blocks(content, "kicad_sch");
    if ranges.is_empty() {
        return Err(DatasheetTargetError::stale(
            "schematic document",
            "the kicad_sch root is missing or malformed",
        ));
    }

    let mut uuid_owners: HashMap<String, Vec<String>> = HashMap::new();
    let mut symbols = Vec::new();
    for (start, end) in ranges {
        let node = parse_sexp(&content[start..end])
            .map_err(|error| DatasheetTargetError::from_sexp("schematic item", error))?;
        let kind = node.head().unwrap_or("unknown");
        if let Some(uuid) = node
            .find("uuid")
            .and_then(|uuid| uuid.get(1))
            .and_then(SexpNode::as_str)
        {
            uuid_owners
                .entry(uuid.to_owned())
                .or_default()
                .push(format!("{kind} at byte {start}"));
        }
        if kind != "symbol" {
            continue;
        }

        let lcsc = direct_property_values(&node, "LCSC");
        if lcsc.is_empty() {
            continue;
        }
        let item = format!("symbol at byte {start}");
        let lcsc_id = one_identity_value(lcsc, "LCSC", &item)?;
        let reference = one_identity_value(
            direct_property_values(&node, "Reference"),
            "Reference",
            &item,
        )?;
        let uuid = node
            .find("uuid")
            .and_then(|uuid| uuid.get(1))
            .and_then(SexpNode::as_str)
            .filter(|uuid| !uuid.trim().is_empty())
            .map(ToOwned::to_owned)
            .ok_or_else(|| DatasheetTargetError::stale(&item, "UUID is missing or empty"))?;
        let units = node.find_all("unit");
        let unit = match units.as_slice() {
            [] => 1,
            [unit] => unit
                .get(1)
                .and_then(SexpNode::as_str)
                .and_then(|unit| unit.parse::<u32>().ok())
                .filter(|unit| *unit > 0)
                .ok_or_else(|| DatasheetTargetError::stale(&item, "unit is invalid"))?,
            _ => {
                return Err(DatasheetTargetError::Ambiguous {
                    target: format!("unit on {item}"),
                    candidates: units
                        .iter()
                        .filter_map(|unit| unit.get(1).and_then(SexpNode::as_str))
                        .map(ToOwned::to_owned)
                        .collect(),
                })
            }
        };
        let datasheets = direct_property_values(&node, "Datasheet");
        let datasheet = match datasheets.as_slice() {
            [] => None,
            [value] => Some(value.clone()),
            _ => {
                return Err(DatasheetTargetError::Ambiguous {
                    target: format!("Datasheet on {reference} unit {unit}"),
                    candidates: datasheets,
                })
            }
        };
        symbols.push(DatasheetSymbolTarget {
            uuid,
            reference,
            unit,
            lcsc_id,
            datasheet,
        });
    }

    for symbol in &symbols {
        if let Some(owners) = uuid_owners
            .get(&symbol.uuid)
            .filter(|owners| owners.len() > 1)
        {
            return Err(DatasheetTargetError::Ambiguous {
                target: format!("schematic UUID {}", symbol.uuid),
                candidates: owners.clone(),
            });
        }
    }

    let mut placed: HashMap<(String, u32), Vec<String>> = HashMap::new();
    for symbol in &symbols {
        placed
            .entry((symbol.reference.clone(), symbol.unit))
            .or_default()
            .push(symbol.uuid.clone());
    }
    if let Some(((reference, unit), uuids)) = placed.iter().find(|(_, uuids)| uuids.len() > 1) {
        return Err(DatasheetTargetError::Ambiguous {
            target: format!("symbol {reference} unit {unit}"),
            candidates: uuids.clone(),
        });
    }

    Ok(symbols)
}

fn plan_datasheet_enrichment(
    content: &str,
    urls: &HashMap<String, String>,
    overwrite: bool,
) -> Result<Option<(SchematicCommand, Vec<PlannedDatasheetUpdate>)>, DatasheetTargetError> {
    let symbols = validated_datasheet_symbols(content)?;
    let mut edited = content.to_owned();
    let mut planned = Vec::new();
    let path = Path::new("datasheet-enrichment.kicad_sch");

    for symbol in symbols {
        let Some(datasheet_url) = urls.get(&symbol.lcsc_id) else {
            continue;
        };
        let Some(existing) = &symbol.datasheet else {
            continue;
        };
        if (!overwrite && existing != "~" && !existing.is_empty()) || existing == datasheet_url {
            continue;
        }
        let id = ItemId::new(symbol.uuid.clone())
            .map_err(|error| DatasheetTargetError::from_sexp(&symbol.reference, error))?;
        let command = SchematicCommand::set_property(
            &edited,
            id,
            "Datasheet",
            datasheet_url,
            format!(
                "enrich datasheet for {} unit {}",
                symbol.reference, symbol.unit
            ),
        )
        .map_err(|error| DatasheetTargetError::from_sexp(&symbol.reference, error))?;
        edited = prepare_command(path, &edited, &command)
            .map_err(|error| DatasheetTargetError::from_sexp(&symbol.reference, error))?
            .0;
        planned.push(PlannedDatasheetUpdate {
            target: symbol,
            datasheet_url: datasheet_url.clone(),
        });
    }

    if planned.is_empty() {
        return Ok(None);
    }
    let ids = planned
        .iter()
        .map(|update| ItemId::new(update.target.uuid.clone()))
        .collect::<Result<Vec<_>, _>>()
        .map_err(|error| DatasheetTargetError::from_sexp("datasheet targets", error))?;
    let command = SchematicCommand::replace_items_from_document(
        content,
        &edited,
        ids,
        "enrich schematic datasheets",
    )
    .map_err(|error| DatasheetTargetError::from_sexp("datasheet targets", error))?
    .requiring_unchanged_document();
    Ok(Some((command, planned)))
}

fn observe_datasheet_updates(
    content: &str,
    planned: &[PlannedDatasheetUpdate],
) -> Result<Vec<serde_json::Value>, DatasheetTargetError> {
    let symbols = validated_datasheet_symbols(content)?;
    let mut observed = Vec::with_capacity(planned.len());
    for update in planned {
        let target = symbols
            .iter()
            .find(|symbol| symbol.uuid == update.target.uuid)
            .ok_or_else(|| {
                DatasheetTargetError::stale(
                    format!("symbol UUID {}", update.target.uuid),
                    "the committed symbol is missing",
                )
            })?;
        if target.reference != update.target.reference
            || target.unit != update.target.unit
            || target.lcsc_id != update.target.lcsc_id
        {
            return Err(DatasheetTargetError::stale(
                format!("symbol UUID {}", update.target.uuid),
                "committed identity differs from the planned reference, unit, or LCSC ID",
            ));
        }
        if target.datasheet.as_deref() != Some(&update.datasheet_url) {
            return Err(DatasheetTargetError::stale(
                format!("symbol {} unit {}", target.reference, target.unit),
                format!(
                    "committed Datasheet was {:?}, expected {}",
                    target.datasheet, update.datasheet_url
                ),
            ));
        }
        observed.push(json!({
            "uuid": target.uuid,
            "reference": target.reference,
            "unit": target.unit,
            "lcsc_id": target.lcsc_id,
            "datasheet_url": target.datasheet
        }));
    }
    Ok(observed)
}

fn datasheet_commit_refusal(path: &Path, error: &SexpError) -> Option<CallToolResult> {
    let reason = match error {
        SexpError::Conflict { .. } => "the schematic changed after enrichment was planned",
        SexpError::ItemConflict { reason, .. } => reason,
        SexpError::KiCadEditorLocked { .. } => {
            "KiCad owns the schematic; use a live editor mutation or close the document"
        }
        _ => return None,
    };
    Some(CallToolResult::error_kind(
        ToolErrorKind::StaleTarget {
            target: path.display().to_string(),
            reason: reason.to_owned(),
        },
        format!(
            "cannot commit datasheet enrichment for {}: {reason}",
            path.display()
        ),
    ))
}

/// The datasheet URL from the local JLCPCB catalog, keyed by LCSC ID or MPN.
/// The database Copperpour already downloads carries the exact LCSC PDF URL in
/// its `Datasheet` column for most parts, so a hit here answers without a
/// network round trip. `Ok(None)` is an ordinary miss; `Err` means the
/// database could not be read at all.
async fn datasheet_from_catalog(
    args: &serde_json::Value,
    ctx: &ToolContext,
    lcsc_id: Option<&str>,
    mpn: Option<&str>,
) -> anyhow::Result<Option<String>> {
    let Some(db_path) = resolve_db_path(args, ctx).filter(|path| path.exists()) else {
        return Ok(None);
    };
    let lcsc_id = lcsc_id.map(String::from);
    let mpn = mpn.map(String::from);
    tokio::task::spawn_blocking(move || -> anyhow::Result<Option<String>> {
        let conn = rusqlite::Connection::open(&db_path)?;
        let (sql, param): (&str, String) = match (&lcsc_id, &mpn) {
            // An LCSC ID is the primary key shape, so it wins when both are
            // given — same precedence the web path used.
            (Some(id), _) => (
                "SELECT Datasheet FROM components WHERE LCSC = ?1 LIMIT 1",
                id.clone(),
            ),
            (None, Some(mpn)) => (
                "SELECT Datasheet FROM components WHERE MFR_Part = ?1 LIMIT 1",
                mpn.clone(),
            ),
            // Unreachable: the caller rejects both-absent before calling.
            (None, None) => return Ok(None),
        };
        let mut stmt = conn.prepare(sql)?;
        let mut rows = stmt.query_map(rusqlite::params![param], |row| row.get::<_, String>(0))?;
        Ok(rows.next().transpose()?.filter(|url| !url.is_empty()))
    })
    .await?
}

async fn handle_get_datasheet_url(
    args: &serde_json::Value,
    ctx: &ToolContext,
) -> anyhow::Result<CallToolResult> {
    let mpn = args["mpn"].as_str();
    let lcsc_id = args["lcsc_id"].as_str();

    if mpn.is_none() && lcsc_id.is_none() {
        return Ok(CallToolResult::error("Provide either 'mpn' or 'lcsc_id'"));
    }

    // The local catalog first: it already holds exact URLs, so a hit saves a
    // network round trip and works offline. Only a miss falls through to the
    // live LCSC API.
    if let Ok(Some(url)) = datasheet_from_catalog(args, ctx, lcsc_id, mpn).await {
        return Ok(CallToolResult::text(
            serde_json::to_string(&json!({
                "mpn": mpn,
                "lcsc_id": lcsc_id,
                "datasheet_url": url,
                "source": "local_catalog"
            }))
            .unwrap(),
        ));
    }

    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(15))
        .build()?;

    // Try LCSC API with lcsc_id first
    if let Some(id) = lcsc_id {
        let url = lcsc_detail_url(args, id);
        if let Ok(resp) = get_with_backoff(&client, &url).await {
            if resp.status().is_success() {
                if let Ok(json_resp) = resp.json::<serde_json::Value>().await {
                    if let Some(ds_url) = json_resp
                        .pointer("/result/dataManualUrl")
                        .and_then(|v| v.as_str())
                    {
                        return Ok(CallToolResult::text(
                            serde_json::to_string(&json!({
                                "lcsc_id": id,
                                "datasheet_url": ds_url,
                                "source": "lcsc_api"
                            }))
                            .unwrap(),
                        ));
                    }
                }
            }
        }
    }

    // Both sources missed — say which were tried, not just that the answer
    // is null (#255).
    let mut tried = vec![];
    match resolve_db_path(&serde_json::Value::Null, ctx) {
        Some(path) if path.exists() => tried.push("local catalog"),
        Some(_) => tried.push("local catalog (database not downloaded)"),
        None => tried.push("local catalog (home directory unknown)"),
    }
    if lcsc_id.is_some() {
        tried.push("LCSC API");
    }
    Ok(CallToolResult::text(
        serde_json::to_string(&json!({
            "mpn": mpn,
            "lcsc_id": lcsc_id,
            "datasheet_url": null,
            "note": format!("Datasheet not found via {}", tried.join(" or "))
        }))
        .unwrap(),
    ))
}

// ─── Freerouting ──────────────────────────────────────────────────────────────

// Discovery, the Java checks and the headless routing session live in
// `autorouting::freerouting` (#209); these handlers keep the tools in this toolset.

async fn handle_check_freerouting(
    args: &serde_json::Value,
    ctx: &ToolContext,
) -> anyhow::Result<CallToolResult> {
    freerouting::check_freerouting(args, ctx).await
}

async fn handle_route_specctra_dsn(
    args: &serde_json::Value,
    ctx: &ToolContext,
) -> anyhow::Result<CallToolResult> {
    freerouting::route_specctra_dsn(args, ctx).await
}

#[cfg(test)]
mod datasheet_enrichment_tests {
    use super::*;
    use crate::mcp::error::extract_error_kind;
    use std::io::Write;

    const KICAD_STRUCTURAL_FIXTURE: &str =
        include_str!("../../tests/fixtures/structural_scans_kicad10.kicad_sch");

    fn symbol(reference: &str, unit: u32, uuid: Option<&str>, lcsc: &str) -> String {
        let uuid = uuid
            .map(|uuid| format!("\r\n\t\t(uuid \"{uuid}\")"))
            .unwrap_or_default();
        format!(
            "\t(symbol\r\n\t\t(lib_id \"Device:R\")\r\n\t\t(at 10 20 0)\r\n\t\t(unit {unit})\r\n\t\t(property \"Reference\" \"{reference}\")\r\n\t\t(property \"Datasheet\" \"~\")\r\n\t\t(property \"Notes\" \"decoy\"\r\n\t\t\t(property \"LCSC\" \"C-DECOY\")\r\n\t\t)\r\n\t\t(property \"LCSC\" \"{lcsc}\"){uuid}\r\n\t)\r\n"
        )
    }

    fn schematic(body: &str) -> String {
        format!("(kicad_sch\r\n\t(version 20231120)\r\n{body})\r\n")
    }

    fn urls(entries: &[(&str, &str)]) -> HashMap<String, String> {
        entries
            .iter()
            .map(|(id, url)| ((*id).to_owned(), (*url).to_owned()))
            .collect()
    }

    #[test]
    fn tab_crlf_multi_unit_symbols_update_atomically_and_read_back() {
        let decoy = "\t(lib_symbols\r\n\t\t(symbol \"decoy\"\r\n\t\t\t(property \"LCSC\" \"C-LIBRARY\")\r\n\t\t)\r\n\t)\r\n";
        let original = schematic(&format!(
            "{decoy}{}{}",
            symbol("U1", 1, Some("unit-1"), "C1"),
            symbol("U1", 2, Some("unit-2"), "C1")
        ));
        let Some((command, planned)) = plan_datasheet_enrichment(
            &original,
            &urls(&[("C1", "https://example.com/u1.pdf")]),
            false,
        )
        .unwrap() else {
            panic!("expected enrichment plan");
        };
        assert_eq!(planned.len(), 2);

        let mut file = tempfile::NamedTempFile::with_suffix(".kicad_sch").unwrap();
        file.write_all(original.as_bytes()).unwrap();
        file.flush().unwrap();
        let outcome = commit_command(file.path(), &command).unwrap();
        assert!(outcome.changed);

        let committed = std::fs::read_to_string(file.path()).unwrap();
        let observed = observe_datasheet_updates(&committed, &planned).unwrap();
        assert_eq!(observed.len(), 2);
        assert_eq!(observed[0]["reference"], "U1");
        assert_eq!(observed[0]["unit"], 1);
        assert_eq!(observed[1]["unit"], 2);
        assert!(observed
            .iter()
            .all(|update| update["datasheet_url"] == "https://example.com/u1.pdf"));
        assert!(committed.contains("\r\n\t\t(property \"Notes\" \"decoy\"\r\n"));
        assert!(committed.contains("(property \"LCSC\" \"C-LIBRARY\")"));
        assert!(committed.contains("(property \"LCSC\" \"C-DECOY\")"));
    }

    #[test]
    fn kicad_authored_symbol_is_enriched_structurally_and_read_back() {
        let Some((command, planned)) = plan_datasheet_enrichment(
            KICAD_STRUCTURAL_FIXTURE,
            &urls(&[("C25804", "https://example.com/c25804.pdf")]),
            false,
        )
        .unwrap() else {
            panic!("expected enrichment plan");
        };
        assert_eq!(planned.len(), 1);
        assert_eq!(planned[0].target.reference, "R1");

        let mut file = tempfile::NamedTempFile::with_suffix(".kicad_sch").unwrap();
        file.write_all(KICAD_STRUCTURAL_FIXTURE.as_bytes()).unwrap();
        file.flush().unwrap();
        let outcome = commit_command(file.path(), &command).unwrap();
        assert!(outcome.changed);

        let committed = std::fs::read_to_string(file.path()).unwrap();
        let observed = observe_datasheet_updates(&committed, &planned).unwrap();
        assert_eq!(observed.len(), 1);
        assert_eq!(observed[0]["reference"], "R1");
        assert_eq!(observed[0]["lcsc_id"], "C25804");
        assert_eq!(
            observed[0]["datasheet_url"],
            "https://example.com/c25804.pdf"
        );
        parse_sexp(&committed).expect("enriched KiCad fixture must still parse");
    }

    #[test]
    fn nested_or_library_lcsc_text_is_not_a_symbol_target() {
        let content = schematic(
            "\t(lib_symbols\n\t\t(symbol \"X\" (property \"LCSC\" \"C1\"))\n\t)\n\t(text \"(property LCSC C1)\" (at 0 0) (uuid \"text-1\"))\n",
        );
        assert!(validated_datasheet_symbols(&content).unwrap().is_empty());
        assert!(plan_datasheet_enrichment(
            &content,
            &urls(&[("C1", "https://example.com/decoy.pdf")]),
            false
        )
        .unwrap()
        .is_none());
    }

    #[test]
    fn missing_uuid_is_a_stale_target() {
        let content = schematic(&symbol("R1", 1, None, "C1"));
        let error = validated_datasheet_symbols(&content).unwrap_err();
        assert!(matches!(error, DatasheetTargetError::Stale { .. }));
    }

    #[test]
    fn duplicate_reference_and_unit_is_a_stale_refusal() {
        let content = schematic(&format!(
            "{}{}",
            symbol("U1", 1, Some("first"), "C1"),
            symbol("U1", 1, Some("second"), "C2")
        ));
        let error = validated_datasheet_symbols(&content).unwrap_err();
        assert!(matches!(&error, DatasheetTargetError::Ambiguous { .. }));
        assert_eq!(
            extract_error_kind(&error.into_result()).as_deref(),
            Some("stale_target")
        );
    }

    #[test]
    fn duplicate_uuid_is_a_stale_refusal_even_across_item_kinds() {
        let content = schematic(&format!(
            "{}\t(junction (at 1 2) (uuid \"shared\"))\r\n",
            symbol("R1", 1, Some("shared"), "C1")
        ));
        let error = validated_datasheet_symbols(&content).unwrap_err();
        assert!(matches!(&error, DatasheetTargetError::Ambiguous { .. }));
        assert_eq!(
            extract_error_kind(&error.into_result()).as_deref(),
            Some("stale_target")
        );
    }

    #[test]
    fn stale_document_revision_refuses_without_overwriting() {
        let original = schematic(&symbol("R1", 1, Some("r1"), "C1"));
        let Some((command, _)) = plan_datasheet_enrichment(
            &original,
            &urls(&[("C1", "https://example.com/r1.pdf")]),
            false,
        )
        .unwrap() else {
            panic!("expected enrichment plan");
        };
        let mut file = tempfile::NamedTempFile::with_suffix(".kicad_sch").unwrap();
        let newer = original.replace("(at 10 20 0)", "(at 11 20 0)");
        file.write_all(newer.as_bytes()).unwrap();
        file.flush().unwrap();

        let error = commit_command(file.path(), &command).unwrap_err();
        let refusal = datasheet_commit_refusal(file.path(), &error).unwrap();
        assert_eq!(
            extract_error_kind(&refusal).as_deref(),
            Some("stale_target")
        );
        assert_eq!(std::fs::read_to_string(file.path()).unwrap(), newer);
    }

    #[test]
    fn overwrite_policy_and_noop_counts_are_truthful() {
        let original = schematic(&symbol("R1", 1, Some("r1"), "C1")).replace(
            "(property \"Datasheet\" \"~\")",
            "(property \"Datasheet\" \"https://existing.example/r1.pdf\")",
        );
        let replacements = urls(&[("C1", "https://new.example/r1.pdf")]);
        assert!(plan_datasheet_enrichment(&original, &replacements, false)
            .unwrap()
            .is_none());
        assert!(plan_datasheet_enrichment(&original, &replacements, true)
            .unwrap()
            .is_some());

        let same = urls(&[("C1", "https://existing.example/r1.pdf")]);
        assert!(plan_datasheet_enrichment(&original, &same, true)
            .unwrap()
            .is_none());
    }

    use crate::router::ToolRouter;
    use crate::tools::ServerConfig;
    use std::sync::Arc;

    fn test_ctx() -> ToolContext {
        ToolContext::new(ServerConfig::default(), Arc::new(ToolRouter::new()))
    }

    fn response_json(result: &CallToolResult) -> serde_json::Value {
        match &result.content[0] {
            crate::mcp::protocol::ToolContent::Text { text } => serde_json::from_str(text).unwrap(),
            _ => panic!("expected text content"),
        }
    }

    fn seed_catalog_db(parts: &[(&str, &str)]) -> (tempfile::TempDir, PathBuf) {
        let dir = tempfile::tempdir().expect("tempdir");
        let db_path = dir.path().join("jlcpcb.db");
        let conn = rusqlite::Connection::open(&db_path).expect("open db");
        conn.execute(
            "CREATE TABLE components (
                LCSC TEXT, MFR_Part TEXT, Package TEXT, Manufacturer TEXT,
                Library_Type TEXT, Description TEXT, Datasheet TEXT,
                Price REAL, Stock INTEGER, Category TEXT
            )",
            [],
        )
        .unwrap();
        for (lcsc, datasheet) in parts {
            conn.execute(
                "INSERT INTO components VALUES (?1, 'MPN', '0402', 'MFR', 'Basic', 'desc', ?2, 0.01, 100, 'cat')",
                rusqlite::params![lcsc, datasheet],
            )
            .unwrap();
        }
        (dir, db_path)
    }

    async fn start_mock_lcsc_server(
        routes: HashMap<String, Option<String>>,
    ) -> (tokio::task::JoinHandle<()>, String) {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let url = format!("http://{addr}/ftps/wm/product/detail");
        let handle = tokio::spawn(async move {
            while let Ok((mut socket, _)) = listener.accept().await {
                use tokio::io::{AsyncReadExt, AsyncWriteExt};
                let mut buf = [0u8; 2048];
                let n = match socket.read(&mut buf).await {
                    Ok(n) if n > 0 => n,
                    _ => continue,
                };
                let req = String::from_utf8_lossy(&buf[..n]);
                let code = req
                    .split_whitespace()
                    .nth(1)
                    .and_then(|path| path.split("productCode=").nth(1))
                    .and_then(|code| code.split('&').next())
                    .unwrap_or("");
                let response = if let Some(Some(ds_url)) = routes.get(code) {
                    let body = format!(r#"{{"result":{{"dataManualUrl":"{ds_url}"}}}}"#);
                    format!("HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len())
                } else {
                    let body = r#"{"result":null}"#;
                    format!("HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len())
                };
                let _ = socket.write_all(response.as_bytes()).await;
            }
        });
        (handle, url)
    }

    #[tokio::test]
    async fn local_catalog_hit_enriches_schematic_without_network_call() {
        let (_dir, db_path) =
            seed_catalog_db(&[("C14663", "https://example.com/c14663-local.pdf")]);
        let ctx = test_ctx();

        let original = schematic(&symbol("R1", 1, Some("r1-uuid"), "C14663"));
        let mut file = tempfile::NamedTempFile::with_suffix(".kicad_sch").unwrap();
        file.write_all(original.as_bytes()).unwrap();
        file.flush().unwrap();

        let result = handle_enrich_datasheets(
            &json!({
                "schematic": file.path().to_str().unwrap(),
                "db_path": db_path.to_str().unwrap()
            }),
            &ctx,
        )
        .await
        .unwrap();

        let body = response_json(&result);
        assert_eq!(body["local_hits"], 1);
        assert_eq!(body["api_hits"], 0);
        assert_eq!(body["misses"], 0);
        assert_eq!(body["datasheets_enriched"], 1);
        assert_eq!(body["skipped_existing_values"], 0);
        assert_eq!(body["attempted_sources"], json!(["local_catalog"]));

        let committed = std::fs::read_to_string(file.path()).unwrap();
        assert!(
            committed.contains(r#"(property "Datasheet" "https://example.com/c14663-local.pdf")"#)
        );
    }

    #[tokio::test]
    async fn empty_local_datasheet_and_catalog_miss_fall_back_to_api() {
        let (_dir, db_path) = seed_catalog_db(&[("C100", "")]);
        let ctx = test_ctx();

        let mut routes = HashMap::new();
        routes.insert(
            "C100".to_string(),
            Some("https://example.com/c100-api.pdf".to_string()),
        );
        routes.insert("C200".to_string(), None);
        let (_server, mock_url) = start_mock_lcsc_server(routes).await;

        let original = schematic(&format!(
            "{}{}",
            symbol("R1", 1, Some("r1-uuid"), "C100"),
            symbol("R2", 1, Some("r2-uuid"), "C200"),
        ));
        let mut file = tempfile::NamedTempFile::with_suffix(".kicad_sch").unwrap();
        file.write_all(original.as_bytes()).unwrap();
        file.flush().unwrap();

        let result = handle_enrich_datasheets(
            &json!({
                "schematic": file.path().to_str().unwrap(),
                "db_path": db_path.to_str().unwrap(),
                "_api_base_url": mock_url
            }),
            &ctx,
        )
        .await
        .unwrap();

        let body = response_json(&result);
        assert_eq!(body["local_hits"], 0);
        assert_eq!(body["api_hits"], 1);
        assert_eq!(body["misses"], 1);
        assert_eq!(body["datasheets_enriched"], 1);
        assert_eq!(body["skipped_existing_values"], 0);
        assert_eq!(
            body["attempted_sources"],
            json!(["local_catalog", "lcsc_api"])
        );

        let committed = std::fs::read_to_string(file.path()).unwrap();
        assert!(committed.contains(r#"(property "Datasheet" "https://example.com/c100-api.pdf")"#));
    }

    #[tokio::test]
    async fn skipped_existing_values_and_multi_unit_component_behavior() {
        let (_dir, db_path) = seed_catalog_db(&[("C500", "https://example.com/c500-new.pdf")]);
        let ctx = test_ctx();

        let original_body = format!(
            "{}{}",
            symbol("U1", 1, Some("u1-1"), "C500"),
            symbol("U1", 2, Some("u1-2"), "C500"),
        )
        .replace(
            r#"(property "Datasheet" "~")"#,
            r#"(property "Datasheet" "https://example.com/u1-existing.pdf")"#,
        );
        let original = schematic(&original_body);

        let mut file = tempfile::NamedTempFile::with_suffix(".kicad_sch").unwrap();
        file.write_all(original.as_bytes()).unwrap();
        file.flush().unwrap();

        // 1. overwrite_existing: false -> preserves existing values on both units
        let result_preserve = handle_enrich_datasheets(
            &json!({
                "schematic": file.path().to_str().unwrap(),
                "db_path": db_path.to_str().unwrap(),
                "overwrite_existing": false
            }),
            &ctx,
        )
        .await
        .unwrap();
        let body_preserve = response_json(&result_preserve);
        assert_eq!(body_preserve["local_hits"], 1);
        assert_eq!(body_preserve["datasheets_enriched"], 0);
        assert_eq!(body_preserve["skipped_existing_values"], 2);

        // 2. overwrite_existing: true -> updates both units consistently
        let result_overwrite = handle_enrich_datasheets(
            &json!({
                "schematic": file.path().to_str().unwrap(),
                "db_path": db_path.to_str().unwrap(),
                "overwrite_existing": true
            }),
            &ctx,
        )
        .await
        .unwrap();
        let body_overwrite = response_json(&result_overwrite);
        assert_eq!(body_overwrite["local_hits"], 1);
        assert_eq!(body_overwrite["datasheets_enriched"], 2);
        assert_eq!(body_overwrite["skipped_existing_values"], 0);

        let committed = std::fs::read_to_string(file.path()).unwrap();
        assert_eq!(
            committed
                .matches(r#"(property "Datasheet" "https://example.com/c500-new.pdf")"#)
                .count(),
            2
        );
    }

    fn ctx_with_catalog(path: PathBuf) -> ToolContext {
        ToolContext::new(
            ServerConfig {
                jlcpcb_db_path: Some(path),
                ..ServerConfig::default()
            },
            Arc::new(ToolRouter::new()),
        )
    }

    fn schematic_file(lcsc_id: &str) -> (tempfile::NamedTempFile, String) {
        let original = schematic(&symbol("R1", 1, Some("r1-uuid"), lcsc_id));
        let mut file = tempfile::NamedTempFile::with_suffix(".kicad_sch").unwrap();
        file.write_all(original.as_bytes()).unwrap();
        file.flush().unwrap();
        (file, original)
    }

    #[tokio::test]
    async fn a_missing_db_path_is_refused_before_any_lookup() {
        let dir = tempfile::tempdir().unwrap();
        let missing = dir.path().join("typo.db");
        let (_server, mock_url) = start_mock_lcsc_server(HashMap::from([(
            "C14663".to_string(),
            Some("https://example.com/c14663-api.pdf".to_string()),
        )]))
        .await;
        let (file, original) = schematic_file("C14663");

        let result = handle_enrich_datasheets(
            &json!({
                "schematic": file.path().to_str().unwrap(),
                "db_path": missing.to_str().unwrap(),
                "_api_base_url": mock_url
            }),
            &test_ctx(),
        )
        .await
        .unwrap();

        assert!(result.is_error);
        let body = response_json(&result);
        assert_eq!(body["error"]["kind"], "file_not_found", "{body}");
        assert_eq!(body["error"]["path"], json!(missing.display().to_string()));
        assert_eq!(std::fs::read_to_string(file.path()).unwrap(), original);
    }

    #[tokio::test]
    async fn an_unreadable_db_path_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        let corrupt = dir.path().join("jlcpcb.db");
        std::fs::write(&corrupt, b"not a sqlite database").unwrap();
        let (file, original) = schematic_file("C14663");

        let result = handle_enrich_datasheets(
            &json!({
                "schematic": file.path().to_str().unwrap(),
                "db_path": corrupt.to_str().unwrap()
            }),
            &test_ctx(),
        )
        .await
        .unwrap();

        assert!(result.is_error);
        let body = response_json(&result);
        assert_eq!(body["error"]["kind"], "file_not_found", "{body}");
        assert_eq!(std::fs::read_to_string(file.path()).unwrap(), original);
    }

    /// A row whose `Datasheet` can't be read as text is skipped and named, and
    /// its part goes to the API, rather than failing the whole catalog. A NULL
    /// one is no datasheet, like an empty one: it goes to the API unnamed.
    #[tokio::test]
    async fn a_datasheet_that_is_not_text_is_skipped_and_reported() {
        let (_dir, db_path) = seed_catalog_db(&[("C100", "https://example.com/c100-local.pdf")]);
        let conn = rusqlite::Connection::open(&db_path).unwrap();
        conn.execute(
            "INSERT INTO components VALUES ('C200', 'MPN', '0402', 'MFR', 'Basic', 'desc', NULL, 0.01, 100, 'cat')",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO components VALUES ('C300', 'MPN', '0402', 'MFR', 'Basic', 'desc', X'00ff', 0.01, 100, 'cat')",
            [],
        )
        .unwrap();
        let (_server, mock_url) = start_mock_lcsc_server(HashMap::from([
            (
                "C200".to_string(),
                Some("https://example.com/c200-api.pdf".to_string()),
            ),
            (
                "C300".to_string(),
                Some("https://example.com/c300-api.pdf".to_string()),
            ),
        ]))
        .await;
        let original = schematic(&format!(
            "{}{}{}",
            symbol("R1", 1, Some("r1-uuid"), "C100"),
            symbol("R2", 1, Some("r2-uuid"), "C200"),
            symbol("R3", 1, Some("r3-uuid"), "C300"),
        ));
        let mut file = tempfile::NamedTempFile::with_suffix(".kicad_sch").unwrap();
        file.write_all(original.as_bytes()).unwrap();
        file.flush().unwrap();

        let result = handle_enrich_datasheets(
            &json!({
                "schematic": file.path().to_str().unwrap(),
                "db_path": db_path.to_str().unwrap(),
                "_api_base_url": mock_url
            }),
            &test_ctx(),
        )
        .await
        .unwrap();

        assert!(!result.is_error);
        let body = response_json(&result);
        assert_eq!(body["local_hits"], 1, "{body}");
        assert_eq!(body["api_hits"], 2, "{body}");
        assert_eq!(body["local_catalog_unreadable_ids"], json!(["C300"]));
        assert_eq!(
            body["attempted_sources"],
            json!(["local_catalog", "lcsc_api"])
        );
        let committed = std::fs::read_to_string(file.path()).unwrap();
        for url in [
            "https://example.com/c100-local.pdf",
            "https://example.com/c200-api.pdf",
            "https://example.com/c300-api.pdf",
        ] {
            assert!(committed.contains(url), "{url} missing");
        }
    }

    async fn served_enrich(
        handler: &crate::mcp::handler::McpHandler,
        arguments: serde_json::Value,
    ) -> CallToolResult {
        let response = handler
            .handle_message(json!({
                "jsonrpc": "2.0",
                "id": 1,
                "method": "tools/call",
                "params": { "name": "enrich_datasheets", "arguments": arguments }
            }))
            .await
            .expect("tools/call receives a response");
        serde_json::from_value(response.result.expect("a result")).unwrap()
    }

    /// `db_path` through served dispatch: the schema gate accepts it, a
    /// missing one is refused, and a misspelt key is refused, the last two
    /// leaving the schematic unchanged.
    #[tokio::test]
    async fn db_path_through_served_dispatch() {
        let handler = crate::mcp::handler::McpHandler::new(ServerConfig {
            eager_toolsets: true,
            ..Default::default()
        })
        .await
        .unwrap();
        let (_dir, db_path) =
            seed_catalog_db(&[("C14663", "https://example.com/c14663-local.pdf")]);

        let (file, original) = schematic_file("C14663");
        let missing = db_path.with_file_name("typo.db");
        for (arguments, kind, field, value) in [
            (
                json!({ "schematic": file.path(), "db_path": missing }),
                "file_not_found",
                "path",
                json!(missing.display().to_string()),
            ),
            (
                json!({ "schematic": file.path(), "db_pth": db_path }),
                "invalid_argument",
                "field",
                json!("db_pth"),
            ),
        ] {
            let result = served_enrich(&handler, arguments.clone()).await;
            assert!(result.is_error, "{arguments}");
            let error = &response_json(&result)["error"];
            assert_eq!(error["kind"], kind, "{arguments}");
            assert_eq!(error[field], value, "{arguments}");
            assert_eq!(
                std::fs::read_to_string(file.path()).unwrap(),
                original,
                "{arguments}"
            );
        }

        let result = served_enrich(
            &handler,
            json!({ "schematic": file.path(), "db_path": db_path }),
        )
        .await;
        assert!(!result.is_error);
        let body = response_json(&result);
        assert_eq!(body["local_hits"], 1, "{body}");
        assert_eq!(body["attempted_sources"], json!(["local_catalog"]));
        assert!(std::fs::read_to_string(file.path())
            .unwrap()
            .contains(r#"(property "Datasheet" "https://example.com/c14663-local.pdf")"#));
    }

    #[tokio::test]
    async fn a_configured_catalog_that_is_absent_is_reported_as_skipped() {
        let dir = tempfile::tempdir().unwrap();
        let ctx = ctx_with_catalog(dir.path().join("jlcpcb.db"));
        let (_server, mock_url) = start_mock_lcsc_server(HashMap::from([(
            "C14663".to_string(),
            Some("https://example.com/c14663-api.pdf".to_string()),
        )]))
        .await;
        let (file, _) = schematic_file("C14663");

        let result = handle_enrich_datasheets(
            &json!({
                "schematic": file.path().to_str().unwrap(),
                "_api_base_url": mock_url
            }),
            &ctx,
        )
        .await
        .unwrap();

        let body = response_json(&result);
        assert_eq!(body["attempted_sources"], json!(["lcsc_api"]));
        assert_eq!(body["api_hits"], 1);
        assert!(body["local_catalog_skipped"].is_string(), "{body}");
        assert!(body.get("local_catalog_error").is_none(), "{body}");
    }

    #[tokio::test]
    async fn a_configured_catalog_that_cannot_be_read_is_reported() {
        let dir = tempfile::tempdir().unwrap();
        let corrupt = dir.path().join("jlcpcb.db");
        std::fs::write(&corrupt, b"not a sqlite database").unwrap();
        let ctx = ctx_with_catalog(corrupt);
        let (_server, mock_url) = start_mock_lcsc_server(HashMap::from([(
            "C14663".to_string(),
            Some("https://example.com/c14663-api.pdf".to_string()),
        )]))
        .await;
        let (file, _) = schematic_file("C14663");

        let result = handle_enrich_datasheets(
            &json!({
                "schematic": file.path().to_str().unwrap(),
                "_api_base_url": mock_url
            }),
            &ctx,
        )
        .await
        .unwrap();

        let body = response_json(&result);
        assert_eq!(body["attempted_sources"], json!(["lcsc_api"]));
        assert_eq!(body["api_hits"], 1);
        assert!(body["local_catalog_error"].is_string(), "{body}");
        assert!(body.get("local_catalog_skipped").is_none(), "{body}");
    }

    #[tokio::test]
    async fn total_miss_provenance_when_not_in_catalog_or_api() {
        let (_dir, db_path) = seed_catalog_db(&[]);
        let ctx = test_ctx();

        let routes = HashMap::new();
        let (_server, mock_url) = start_mock_lcsc_server(routes).await;

        let original = schematic(&symbol("R1", 1, Some("r1"), "C9999"));
        let mut file = tempfile::NamedTempFile::with_suffix(".kicad_sch").unwrap();
        file.write_all(original.as_bytes()).unwrap();
        file.flush().unwrap();

        let result = handle_enrich_datasheets(
            &json!({
                "schematic": file.path().to_str().unwrap(),
                "db_path": db_path.to_str().unwrap(),
                "_api_base_url": mock_url
            }),
            &ctx,
        )
        .await
        .unwrap();

        let body = response_json(&result);
        assert_eq!(body["local_hits"], 0);
        assert_eq!(body["api_hits"], 0);
        assert_eq!(body["misses"], 1);
        assert_eq!(body["datasheets_enriched"], 0);
        assert_eq!(
            body["attempted_sources"],
            json!(["local_catalog", "lcsc_api"])
        );
    }
}

#[cfg(test)]
mod jlcpcb_cache_tests {
    use super::*;
    use crate::router::ToolRouter;
    use crate::tools::ServerConfig;
    use std::sync::Arc;

    fn test_ctx() -> ToolContext {
        ToolContext::new(
            ServerConfig {
                kicad_cli: String::new(),
                kicad_binary: String::new(),
                ipc_address: String::new(),
                project_dir: None,
                jlcpcb_db_path: None,
                auto_load_toolsets: false,
                eager_toolsets: false,
            },
            Arc::new(ToolRouter::new()),
        )
    }

    /// Builds a temp SQLite file with a `components` table matching the
    /// schema the handlers query, seeded with one part.
    fn seed_test_db() -> (tempfile::TempDir, std::path::PathBuf) {
        let dir = tempfile::tempdir().expect("tempdir");
        let db_path = dir.path().join("jlcpcb.db");
        let conn = rusqlite::Connection::open(&db_path).expect("open db");
        conn.execute(
            "CREATE TABLE components (
                LCSC TEXT, MFR_Part TEXT, Package TEXT, Manufacturer TEXT,
                Library_Type TEXT, Description TEXT, Datasheet TEXT,
                Price REAL, Stock INTEGER, Category TEXT
            )",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO components VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
            rusqlite::params![
                "C14663",
                "RC0402FR-0710KL",
                "0402",
                "YAGEO",
                "Basic",
                "10k resistor 0402",
                "https://www.lcsc.com/datasheet/C14663.pdf",
                0.01,
                5000,
                "Resistors / Chip Resistor - Surface Mount"
            ],
        )
        .unwrap();
        (dir, db_path)
    }

    #[tokio::test]
    async fn search_jlcpcb_parts_caches_repeated_query() {
        let (_dir, db_path) = seed_test_db();
        let ctx = test_ctx();
        let args = json!({
            "query": "10k",
            "output_path": db_path.to_str().unwrap()
        });

        let first = handle_search_jlcpcb_parts(&args, &ctx).await.unwrap();
        let second = handle_search_jlcpcb_parts(&args, &ctx).await.unwrap();

        let first_body = response_json(&first);
        let second_body = response_json(&second);
        assert_eq!(first_body["cached"], json!(false));
        assert_eq!(second_body["cached"], json!(true));
        assert_eq!(first_body["results"], second_body["results"]);
        assert_eq!(first_body["count"], json!(1));
    }

    /// A temp database holding neighbouring capacitor and resistor values.
    fn seed_value_db() -> (tempfile::TempDir, std::path::PathBuf) {
        let (dir, db_path) = seed_test_db();
        let conn = rusqlite::Connection::open(&db_path).unwrap();
        for (lcsc, mpn, package, description) in [
            (
                "C1553",
                "CL05C200JB5NNNC",
                "0402",
                "50V 20pF C0G ±5% 0402 Multilayer Ceramic Capacitors",
            ),
            (
                "C1557",
                "0402CG220J500NT",
                "0402",
                "50V 220pF C0G ±5% 0402 Multilayer Ceramic Capacitors",
            ),
            (
                "C1561",
                "0402CG121J500NT",
                "0402",
                "50V 120pF C0G ±5% 0402 Multilayer Ceramic Capacitors",
            ),
            (
                "C11702",
                "0402WGF1001TCE",
                "0402",
                "1kΩ ±1% 62.5mW 0402 Chip Resistor",
            ),
            (
                "C25543",
                "0402WGF1101TCE",
                "0402",
                "1.1kΩ ±1% 62.5mW 0402 Chip Resistor",
            ),
        ] {
            conn.execute(
                "INSERT INTO components VALUES (?1, ?2, ?3, 'FH', 'Basic', ?4, '', 0.01, 1000, 'Capacitors')",
                rusqlite::params![lcsc, mpn, package, description],
            )
            .unwrap();
        }
        (dir, db_path)
    }

    async fn search_lcsc(db_path: &std::path::Path, query: &str) -> Vec<String> {
        let args = json!({ "query": query, "output_path": db_path.to_str().unwrap() });
        let body = response_json(
            &handle_search_jlcpcb_parts(&args, &test_ctx())
                .await
                .unwrap(),
        );
        body["results"]
            .as_array()
            .unwrap()
            .iter()
            .map(|part| part["lcsc"].as_str().unwrap().to_string())
            .collect()
    }

    /// `20pF` used to find `220pF` and `120pF`, and `1k` found `1.1k` and
    /// `10k`, because each word was a bare substring (#786).
    #[tokio::test]
    async fn search_jlcpcb_parts_matches_values_whole() {
        let (_dir, db_path) = seed_value_db();
        assert_eq!(search_lcsc(&db_path, "20pF").await, ["C1553"]);
        assert_eq!(search_lcsc(&db_path, "20p 0402").await, ["C1553"]);
        assert_eq!(search_lcsc(&db_path, "20 pF").await, ["C1553"]);
        assert_eq!(search_lcsc(&db_path, "1k").await, ["C11702"]);
        assert_eq!(search_lcsc(&db_path, "1kΩ 0402").await, ["C11702"]);
    }

    /// Words that are not values keep substring matching, so an MPN prefix
    /// that starts with a digit still finds the full part number.
    #[tokio::test]
    async fn search_jlcpcb_parts_keeps_substring_match_for_other_words() {
        let (_dir, db_path) = seed_value_db();
        assert_eq!(search_lcsc(&db_path, "0402CG").await, ["C1557", "C1561"]);
        assert_eq!(search_lcsc(&db_path, "0402WGF").await, ["C11702", "C25543"]);
        assert_eq!(search_lcsc(&db_path, "Ceramic").await.len(), 3);
    }

    /// The limit counts parts that pass the whole-value check, so a page of
    /// rejected neighbours never leaves the result short.
    #[tokio::test]
    async fn search_jlcpcb_parts_limit_counts_whole_value_matches() {
        let (_dir, db_path) = seed_value_db();
        let args = json!({ "query": "20pF", "limit": 1, "output_path": db_path.to_str().unwrap() });
        let body = response_json(
            &handle_search_jlcpcb_parts(&args, &test_ctx())
                .await
                .unwrap(),
        );
        assert_eq!(body["count"], 1, "{body}");
        assert_eq!(body["results"][0]["lcsc"], "C1553", "{body}");
        assert_eq!(body["value_tokens"], json!(["20pF"]), "{body}");
    }

    #[test]
    fn value_tokens_are_numbers_with_an_optional_prefix_and_unit() {
        for token in [
            "20pF", "20p", "20P", "4.7uF", "1k", "10kΩ", "16V", "8MHz", "0402", "100",
        ] {
            assert!(is_jlcpcb_value_token(token), "{token}");
        }
        for token in [
            "pF",
            "0402CG",
            "0603WAF",
            "4k7",
            "STM32F411",
            "LQFP-48",
            "C1553",
        ] {
            assert!(!is_jlcpcb_value_token(token), "{token}");
        }
    }

    #[tokio::test]
    async fn search_jlcpcb_parts_different_query_is_a_cache_miss() {
        let (_dir, db_path) = seed_test_db();
        let ctx = test_ctx();

        let args_a = json!({ "query": "10k", "output_path": db_path.to_str().unwrap() });
        let args_b = json!({ "query": "100nF", "output_path": db_path.to_str().unwrap() });

        handle_search_jlcpcb_parts(&args_a, &ctx).await.unwrap();
        let second = handle_search_jlcpcb_parts(&args_b, &ctx).await.unwrap();

        assert_eq!(response_json(&second)["cached"], json!(false));
    }

    #[tokio::test]
    async fn get_jlcpcb_part_caches_repeated_lookup() {
        let (_dir, db_path) = seed_test_db();
        let ctx = test_ctx();
        let args = json!({
            "lcsc_id": "C14663",
            "output_path": db_path.to_str().unwrap()
        });

        let first = handle_get_jlcpcb_part(&args, &ctx).await.unwrap();
        let second = handle_get_jlcpcb_part(&args, &ctx).await.unwrap();

        assert_eq!(response_json(&first)["cached"], json!(false));
        assert_eq!(response_json(&second)["cached"], json!(true));
        assert_eq!(response_json(&first)["lcsc"], json!("C14663"));
    }

    #[tokio::test]
    async fn suggest_alternatives_caches_repeated_query() {
        let (_dir, db_path) = seed_test_db();
        let ctx = test_ctx();
        let args = json!({
            "value": "10k",
            "footprint": "Resistor_SMD:R_0402",
            "output_path": db_path.to_str().unwrap()
        });

        let first = handle_suggest_alternatives(&args, &ctx).await.unwrap();
        let second = handle_suggest_alternatives(&args, &ctx).await.unwrap();

        assert_eq!(response_json(&first)["cached"], json!(false));
        assert_eq!(response_json(&second)["cached"], json!(true));
    }

    /// Seeds parts whose search words are spread over different columns.
    fn seed_multi_word_db() -> (tempfile::TempDir, std::path::PathBuf) {
        let (dir, db_path) = seed_test_db();
        let conn = rusqlite::Connection::open(&db_path).expect("open db");
        for (lcsc, mpn, package, mfr, desc) in [
            (
                "C1001",
                "PH-1x7",
                "2.54mm",
                "BOOMELE",
                "Pin Header 1x7 Through Hole Male",
            ),
            (
                "C1002",
                "PH-1x40",
                "2.54mm",
                "BOOMELE",
                "40P Pin Header Through Hole",
            ),
            (
                "C1003",
                "CAP-50",
                "0603",
                "Samsung",
                "50% duty_cycle capacitor",
            ),
            (
                "C1004",
                "CAP-51",
                "0603",
                "Samsung",
                "50 duty cycle capacitor",
            ),
        ] {
            conn.execute(
                "INSERT INTO components VALUES (?1, ?2, ?3, ?4, 'Extended', ?5, '', 0.1, 10, 'Misc')",
                rusqlite::params![lcsc, mpn, package, mfr, desc],
            )
            .unwrap();
        }
        (dir, db_path)
    }

    async fn search_lcsc_ids(db_path: &std::path::Path, query: &str) -> Vec<String> {
        let ctx = test_ctx();
        let args = json!({ "query": query, "output_path": db_path.to_str().unwrap() });
        let result = handle_search_jlcpcb_parts(&args, &ctx).await.unwrap();
        let mut ids: Vec<String> = response_json(&result)["results"]
            .as_array()
            .unwrap()
            .iter()
            .map(|r| r["lcsc"].as_str().unwrap().to_string())
            .collect();
        ids.sort();
        ids
    }

    /// The defect in #432: the whole query was bound as one `LIKE '%query%'`
    /// phrase against Description/MFR_Part, so reordering, trimming or adding
    /// a word that is not in that exact order flipped a hit into zero results.
    #[tokio::test]
    async fn search_jlcpcb_parts_matches_every_token_in_any_order() {
        let (_dir, db_path) = seed_multi_word_db();
        assert_eq!(
            search_lcsc_ids(&db_path, "Pin Header 1x7").await,
            vec!["C1001"]
        );
        assert_eq!(
            search_lcsc_ids(&db_path, "  1x7   header pin ").await,
            vec!["C1001"]
        );
        assert_eq!(
            search_lcsc_ids(&db_path, "through hole pin header").await,
            vec!["C1001", "C1002"]
        );
        assert_eq!(
            search_lcsc_ids(&db_path, "40P Pin Header Through Hole").await,
            vec!["C1002"]
        );
    }

    #[tokio::test]
    async fn search_jlcpcb_parts_punctuation_normalized_mpn_fallback() {
        let (_dir, db_path) = seed_test_db();
        let conn = rusqlite::Connection::open(&db_path).unwrap();
        // Insert parts with hyphens, underscores, etc.
        conn.execute(
            "INSERT INTO components VALUES ('C2856837', 'FPC-05FB-40PH20', '0.5mm', 'XUNPU', 'Extended', '0.5mm Pitch 40 Pin FPC Connector', '', 0.5, 100, 'Connectors')",
            [],
        ).unwrap();
        conn.execute(
            "INSERT INTO components VALUES ('C8734', 'STM32F103-C8T6', 'LQFP-48', 'STMicroelectronics', 'Extended', '32-bit ARM Cortex-M3 MCU', '', 1.5, 50, 'Microcontrollers')",
            [],
        ).unwrap();

        // 1. Unhyphenated MPN query finds hyphenated part
        assert_eq!(
            search_lcsc_ids(&db_path, "STM32F103C8T6").await,
            vec!["C8734"]
        );
        assert_eq!(
            search_lcsc_ids(&db_path, "05FB40PH20").await,
            vec!["C2856837"]
        );
        assert_eq!(
            search_lcsc_ids(&db_path, "FPC05FB40PH20").await,
            vec!["C2856837"]
        );

        // 2. URL slug combining manufacturer and MPN with separators
        assert_eq!(
            search_lcsc_ids(&db_path, "XUNPU-FPC_05FB40PH20").await,
            vec!["C2856837"]
        );

        // 3. Exact query still works directly
        assert_eq!(
            search_lcsc_ids(&db_path, "FPC-05FB-40PH20").await,
            vec!["C2856837"]
        );
        assert_eq!(
            search_lcsc_ids(&db_path, "STM32F103-C8T6").await,
            vec!["C8734"]
        );
    }

    #[tokio::test]
    async fn search_jlcpcb_parts_tokens_may_span_columns() {
        let (_dir, db_path) = seed_multi_word_db();
        // Package, Manufacturer, LCSC and MFR_Part are all searchable.
        assert_eq!(
            search_lcsc_ids(&db_path, "boomele 2.54mm 1x7").await,
            vec!["C1001"]
        );
        assert_eq!(
            search_lcsc_ids(&db_path, "C1002 header").await,
            vec!["C1002"]
        );
        assert_eq!(
            search_lcsc_ids(&db_path, "PH-1x40 boomele").await,
            vec!["C1002"]
        );
    }

    #[tokio::test]
    async fn search_jlcpcb_parts_absent_token_returns_no_row() {
        let (_dir, db_path) = seed_multi_word_db();
        assert!(search_lcsc_ids(&db_path, "pin header 1x7 nonexistentword")
            .await
            .is_empty());
    }

    /// The filters and LIMIT keep working alongside the tokenised match.
    #[tokio::test]
    async fn search_jlcpcb_parts_preserves_stock_basic_category_and_limit_filters() {
        let (_dir, db_path) = seed_multi_word_db();
        let conn = rusqlite::Connection::open(&db_path).expect("open db");
        for (lcsc, lib, desc, stock, cat) in [
            ("C2001", "Basic", "Widget gadget alpha", 10, "Gadgets"),
            ("C2002", "Extended", "Widget gadget beta", 10, "Gadgets"),
            ("C2003", "Basic", "Widget gadget gamma", 0, "Gadgets"),
            ("C2004", "Basic", "Widget gadget delta", 10, "Other"),
        ] {
            conn.execute(
                "INSERT INTO components VALUES (?1, 'MPN', 'PKG', 'MFR', ?2, ?3, '', 0.1, ?4, ?5)",
                rusqlite::params![lcsc, lib, desc, stock, cat],
            )
            .unwrap();
        }
        let ctx = test_ctx();
        let run = |extra: serde_json::Value| {
            let mut args =
                json!({ "query": "gadget widget", "output_path": db_path.to_str().unwrap() });
            for (k, v) in extra.as_object().unwrap() {
                args[k] = v.clone();
            }
            let ctx = &ctx;
            async move {
                let result = handle_search_jlcpcb_parts(&args, ctx).await.unwrap();
                let mut ids: Vec<String> = response_json(&result)["results"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .map(|r| r["lcsc"].as_str().unwrap().to_string())
                    .collect();
                ids.sort();
                ids
            }
        };
        // in_stock defaults to true: C2003 (stock 0) is excluded.
        assert_eq!(run(json!({})).await, vec!["C2001", "C2002", "C2004"]);
        assert_eq!(
            run(json!({ "in_stock": false })).await,
            vec!["C2001", "C2002", "C2003", "C2004"]
        );
        assert_eq!(
            run(json!({ "basic_only": true })).await,
            vec!["C2001", "C2004"]
        );
        assert_eq!(
            run(json!({ "category": "Gadgets" })).await,
            vec!["C2001", "C2002"]
        );
        assert_eq!(
            run(json!({ "category": "Gadgets", "basic_only": true, "in_stock": false })).await,
            vec!["C2001", "C2003"]
        );
        assert_eq!(run(json!({ "limit": 2 })).await.len(), 2);
    }

    #[tokio::test]
    async fn search_jlcpcb_parts_like_metacharacters_are_literal() {
        let (_dir, db_path) = seed_multi_word_db();
        assert_eq!(search_lcsc_ids(&db_path, "50%").await, vec!["C1003"]);
        assert_eq!(search_lcsc_ids(&db_path, "duty_cycle").await, vec!["C1003"]);
        assert!(search_lcsc_ids(&db_path, "%").await.len() == 1);
    }

    /// The seed part (`Resistors / Chip Resistor - Surface Mount`), plus a
    /// group with two subcategories, a category with no subcategory, and a
    /// part with none at all.
    fn seed_category_db() -> (tempfile::TempDir, std::path::PathBuf) {
        let (dir, db_path) = seed_test_db();
        let conn = rusqlite::Connection::open(&db_path).expect("open db");
        for (lcsc, stock, category) in [
            ("C3001", 10, "Optoelectronics / LED Indication - Discrete"),
            ("C3002", 0, "Optoelectronics / LED Indication - Discrete"),
            ("C3003", 5, "Optoelectronics / RGB LEDs"),
            ("C3004", 1, "Misc"),
            ("C3005", 1, ""),
        ] {
            conn.execute(
                "INSERT INTO components VALUES (?1, 'MPN', 'PKG', 'MFR', 'Extended', 'part', '', 0.1, ?2, ?3)",
                rusqlite::params![lcsc, stock, category],
            )
            .unwrap();
        }
        (dir, db_path)
    }

    #[tokio::test]
    async fn list_jlcpcb_categories_lists_top_level_groups() {
        let (_dir, db_path) = seed_category_db();
        let result = handle_list_jlcpcb_categories(
            &json!({ "output_path": db_path.to_str().unwrap() }),
            &test_ctx(),
        )
        .await
        .unwrap();
        let body = response_json(&result);
        assert_eq!(body["count"], 3, "{body}");
        assert_eq!(
            body["categories"],
            json!([
                { "category": "Misc", "part_count": 1, "in_stock_count": 1, "subcategory_count": 0 },
                { "category": "Optoelectronics", "part_count": 3, "in_stock_count": 2, "subcategory_count": 2 },
                { "category": "Resistors", "part_count": 1, "in_stock_count": 1, "subcategory_count": 1 }
            ])
        );
    }

    #[tokio::test]
    async fn list_jlcpcb_categories_contains_lists_full_names() {
        let (_dir, db_path) = seed_category_db();
        let list = |contains: &'static str| {
            let args = json!({ "output_path": db_path.to_str().unwrap(), "contains": contains });
            async move {
                response_json(
                    &handle_list_jlcpcb_categories(&args, &test_ctx())
                        .await
                        .unwrap(),
                )
            }
        };
        let body = list("led").await;
        assert_eq!(body["count"], 2, "{body}");
        assert_eq!(
            body["categories"],
            json!([
                { "category": "Optoelectronics / LED Indication - Discrete", "part_count": 2, "in_stock_count": 1 },
                { "category": "Optoelectronics / RGB LEDs", "part_count": 1, "in_stock_count": 1 }
            ])
        );
        assert_eq!(list("Light Emitting Diodes").await["count"], 0);
    }

    #[tokio::test]
    async fn list_jlcpcb_categories_without_a_database_refuses() {
        let dir = tempfile::tempdir().unwrap();
        let result = handle_list_jlcpcb_categories(
            &json!({ "output_path": dir.path().join("jlcpcb.db").to_str().unwrap() }),
            &test_ctx(),
        )
        .await
        .unwrap();
        assert!(result.is_error);
    }

    #[tokio::test]
    async fn jlcpcb_stats_counts_categories() {
        let (_dir, db_path) = seed_category_db();
        let result = handle_jlcpcb_stats(
            &json!({ "output_path": db_path.to_str().unwrap() }),
            &test_ctx(),
        )
        .await
        .unwrap();
        let body = response_json(&result);
        assert_eq!(body["part_count"], 6, "{body}");
        assert_eq!(body["category_count"], 4, "{body}");
    }

    #[tokio::test]
    async fn search_jlcpcb_parts_category_filter_is_literal_and_case_insensitive() {
        let (_dir, db_path) = seed_category_db();
        let conn = rusqlite::Connection::open(&db_path).expect("open db");
        for (lcsc, category) in [("C3101", "Gadgets_A"), ("C3102", "GadgetsXA")] {
            conn.execute(
                "INSERT INTO components VALUES (?1, 'MPN', 'PKG', 'MFR', 'Extended', 'widget', '', 0.1, 1, ?2)",
                rusqlite::params![lcsc, category],
            )
            .unwrap();
        }
        let search = |category: &'static str| {
            let args = json!({
                "query": "widget",
                "category": category,
                "output_path": db_path.to_str().unwrap()
            });
            async move {
                let result = handle_search_jlcpcb_parts(&args, &test_ctx())
                    .await
                    .unwrap();
                response_json(&result)["results"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .map(|r| r["lcsc"].as_str().unwrap().to_string())
                    .collect::<Vec<_>>()
            }
        };
        assert_eq!(search("gadgets_a").await, vec!["C3101"]);
        assert_eq!(search("Gadgets").await, vec!["C3101", "C3102"]);
    }

    /// The punctuation-normalised fallback filters by category the same way:
    /// `FALLBACK1` finds `FALL-BACK1` only there.
    #[tokio::test]
    async fn search_jlcpcb_parts_fallback_category_filter_is_literal_and_case_insensitive() {
        let (_dir, db_path) = seed_category_db();
        let conn = rusqlite::Connection::open(&db_path).expect("open db");
        for (lcsc, category) in [("C3201", "Gadgets_A"), ("C3202", "GadgetsXA")] {
            conn.execute(
                "INSERT INTO components VALUES (?1, 'FALL-BACK1', 'PKG', 'MFR', 'Extended', 'gizmo', '', 0.1, 1, ?2)",
                rusqlite::params![lcsc, category],
            )
            .unwrap();
        }
        let search = |category: &'static str| {
            let args = json!({
                "query": "FALLBACK1",
                "category": category,
                "output_path": db_path.to_str().unwrap()
            });
            async move {
                let result = handle_search_jlcpcb_parts(&args, &test_ctx())
                    .await
                    .unwrap();
                response_json(&result)["results"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .map(|r| r["lcsc"].as_str().unwrap().to_string())
                    .collect::<Vec<_>>()
            }
        };
        assert_eq!(search("gadgets_a").await, vec!["C3201"]);
        assert_eq!(search("Gadgets").await, vec!["C3201", "C3202"]);
    }

    fn response_json(result: &CallToolResult) -> serde_json::Value {
        match &result.content[0] {
            crate::mcp::protocol::ToolContent::Text { text } => serde_json::from_str(text).unwrap(),
            _ => panic!("expected text content"),
        }
    }

    /// The defect in #255: the local catalog holds exact datasheet URLs, but
    /// get_datasheet_url only ever asked the live LCSC API and returned null.
    /// A catalog hit must answer directly — no network involved.
    #[tokio::test]
    async fn get_datasheet_url_answers_from_the_local_catalog() {
        let (_dir, db_path) = seed_test_db();
        let ctx = test_ctx();

        let by_lcsc = handle_get_datasheet_url(
            &json!({ "lcsc_id": "C14663", "output_path": db_path.to_str().unwrap() }),
            &ctx,
        )
        .await
        .unwrap();
        let body = response_json(&by_lcsc);
        assert_eq!(
            body["datasheet_url"],
            json!("https://www.lcsc.com/datasheet/C14663.pdf")
        );
        assert_eq!(body["source"], json!("local_catalog"));

        let by_mpn = handle_get_datasheet_url(
            &json!({ "mpn": "RC0402FR-0710KL", "output_path": db_path.to_str().unwrap() }),
            &ctx,
        )
        .await
        .unwrap();
        assert_eq!(
            response_json(&by_mpn)["datasheet_url"],
            json!("https://www.lcsc.com/datasheet/C14663.pdf")
        );
    }

    /// The catalog projections hid the column too — a caller could not get a
    /// URL from part lookup either.
    #[tokio::test]
    async fn catalog_lookups_carry_the_datasheet_url() {
        let (_dir, db_path) = seed_test_db();
        let ctx = test_ctx();

        let part = handle_get_jlcpcb_part(
            &json!({ "lcsc_id": "C14663", "output_path": db_path.to_str().unwrap() }),
            &ctx,
        )
        .await
        .unwrap();
        assert_eq!(
            response_json(&part)["datasheet_url"],
            json!("https://www.lcsc.com/datasheet/C14663.pdf")
        );

        let search = handle_search_jlcpcb_parts(
            &json!({ "query": "10k", "output_path": db_path.to_str().unwrap() }),
            &ctx,
        )
        .await
        .unwrap();
        assert_eq!(
            response_json(&search)["results"][0]["datasheet_url"],
            json!("https://www.lcsc.com/datasheet/C14663.pdf")
        );
    }

    #[tokio::test]
    async fn search_jlcpcb_parts_ties_are_ordered_by_lcsc() {
        let (_dir, db_path) = seed_multi_word_db();
        let ctx = test_ctx();
        // Insert out of LCSC order so rowid order differs from LCSC order.
        let conn = rusqlite::Connection::open(&db_path).unwrap();
        for lcsc in ["C3003", "C3001", "C3002"] {
            conn.execute(
                "INSERT INTO components VALUES (?1, 'ORD-1', '0805', 'Acme', 'Basic', 'ordercheck part', '', 0.1, 10, 'Misc')",
                rusqlite::params![lcsc],
            )
            .unwrap();
        }
        let args = json!({
            "query": "ordercheck",
            "limit": 2,
            "output_path": db_path.to_str().unwrap()
        });
        let body = response_json(&handle_search_jlcpcb_parts(&args, &ctx).await.unwrap());
        let ids: Vec<&str> = body["results"]
            .as_array()
            .unwrap()
            .iter()
            .map(|r| r["lcsc"].as_str().unwrap())
            .collect();
        assert_eq!(ids, ["C3001", "C3002"]);
    }

    fn ranked_ids(body: &serde_json::Value) -> Vec<String> {
        body["results"]
            .as_array()
            .unwrap()
            .iter()
            .map(|r| r["lcsc"].as_str().unwrap().to_string())
            .collect()
    }

    /// Search results rank in-stock parts first, then as
    /// suggest_jlcpcb_alternatives ranks them (#90): library type, then known
    /// price, price, stock, LCSC number.
    #[tokio::test]
    async fn search_jlcpcb_parts_ranks_in_stock_first_then_like_suggest_alternatives() {
        let (_dir, db_path) = seed_multi_word_db();
        let conn = rusqlite::Connection::open(&db_path).unwrap();
        for (lcsc, library_type, price, stock) in [
            ("C4001", "Extended", Some(0.01), 1000),
            ("C4002", "Basic", Some(0.05), 10),
            ("C4003", "Preferred", Some(0.02), 10),
            ("C4004", "Basic", Some(0.0), 999_999),
            ("C4005", "Basic", Some(0.05), 500),
            ("C4006", "Basic", Some(0.001), 0),
            // An unknown price ranks with a 0 price, by stock (#63).
            ("C4007", "Basic", None, 5_000_000),
        ] {
            conn.execute(
                "INSERT INTO components VALUES (?1, 'RNK-1', '0805', 'Acme', ?2, 'rankcheck part', '', ?3, ?4, 'Misc')",
                rusqlite::params![lcsc, library_type, price, stock],
            )
            .unwrap();
        }
        let search = |in_stock: bool| {
            let args = json!({
                "query": "rankcheck",
                "in_stock": in_stock,
                "output_path": db_path.to_str().unwrap()
            });
            async move {
                response_json(
                    &handle_search_jlcpcb_parts(&args, &test_ctx())
                        .await
                        .unwrap(),
                )
            }
        };
        let body = search(true).await;
        assert_eq!(
            ranked_ids(&body),
            ["C4005", "C4002", "C4007", "C4004", "C4003", "C4001"]
        );
        // The order the response promises is public, so it is restated here
        // rather than read back from the code that builds it.
        assert_eq!(
            body["ranking"],
            json!([
                "in stock before out of stock",
                "library_type: Basic, Preferred, Extended",
                "known price before unknown price",
                "price ascending",
                "stock descending",
                "LCSC number",
            ])
        );
        // The cheapest Basic part has no stock, so it ranks after every
        // stocked part, Extended included.
        assert_eq!(
            ranked_ids(&search(false).await),
            ["C4005", "C4002", "C4007", "C4004", "C4003", "C4001", "C4006"]
        );
    }

    /// The punctuation-normalised fallback ranks the same way.
    #[tokio::test]
    async fn search_jlcpcb_parts_fallback_ranks_like_suggest_alternatives() {
        let (_dir, db_path) = seed_multi_word_db();
        let conn = rusqlite::Connection::open(&db_path).unwrap();
        for (lcsc, library_type) in [("C4101", "Extended"), ("C4102", "Basic")] {
            conn.execute(
                "INSERT INTO components VALUES (?1, 'RANK-X1', '0805', 'Acme', ?2, 'part', '', 0.1, 10, 'Misc')",
                rusqlite::params![lcsc, library_type],
            )
            .unwrap();
        }
        let args = json!({ "query": "RANKX1", "output_path": db_path.to_str().unwrap() });
        let body = response_json(
            &handle_search_jlcpcb_parts(&args, &test_ctx())
                .await
                .unwrap(),
        );
        assert_eq!(ranked_ids(&body), ["C4102", "C4101"]);
    }

    #[tokio::test]
    async fn served_tools_call_reports_tokens_and_fields_on_miss_and_cache_hit() {
        let (_dir, db_path) = seed_multi_word_db();
        let handler = crate::mcp::handler::McpHandler::new(ServerConfig {
            kicad_cli: String::new(),
            kicad_binary: String::new(),
            ipc_address: String::new(),
            project_dir: None,
            jlcpcb_db_path: Some(db_path.clone()),
            auto_load_toolsets: true,
            eager_toolsets: true,
        })
        .await
        .expect("handler builds");
        let call = || async {
            let response = handler
                .handle_message(json!({
                    "jsonrpc": "2.0",
                    "id": 1,
                    "method": "tools/call",
                    "params": {
                        "name": "search_jlcpcb_parts",
                        "arguments": {
                            "query": "  header   Pin 1x7 ",
                            "limit": 5
                        }
                    }
                }))
                .await
                .expect("response");
            let result = response.result.expect("result");
            serde_json::from_str::<serde_json::Value>(
                result["content"][0]["text"].as_str().expect("text"),
            )
            .expect("json body")
        };
        let fresh = call().await;
        let hit = call().await;
        assert_eq!(fresh["cached"], json!(false));
        assert_eq!(hit["cached"], json!(true));
        for body in [&fresh, &hit] {
            assert_eq!(body["tokens"], json!(["header", "Pin", "1x7"]));
            assert_eq!(
                body["search_fields"],
                json!(["Description", "MFR_Part", "LCSC", "Package", "Manufacturer"])
            );
            assert_eq!(body["query"], json!("  header   Pin 1x7 "));
        }
        assert_eq!(fresh["results"], hit["results"]);
        assert!(fresh["count"].as_u64().unwrap() >= 1);
    }

    /// With no database at all the tool degrades to its old behaviour — web
    // lookup (unreachable in tests), then null with a note naming what was
    // tried rather than blaming only the API (#255).
    #[tokio::test]
    async fn a_catalog_miss_falls_through_and_the_note_names_the_sources() {
        let ctx = test_ctx(); // jlcpcb_db_path: None → default path absent

        let result = handle_get_datasheet_url(&json!({ "lcsc_id": "C99999999" }), &ctx)
            .await
            .unwrap();
        let body = response_json(&result);
        assert_eq!(body["datasheet_url"], serde_json::Value::Null);
        let note = body["note"].as_str().unwrap();
        assert!(note.contains("local catalog"), "{note}");
    }
}

#[cfg(test)]
mod suggest_alternatives_tests {
    use super::*;
    use crate::tools::ServerConfig;

    /// Real rows of the Copperpour-built catalogue (`components` table), copied
    /// verbatim from a `download_jlcpcb_database` run on 2026-10-03. The
    /// README beside it records the source and how the rows were chosen.
    const FIXTURE: &str = include_str!("../../tests/fixtures/jlcpcb/suggest_alternatives.tsv");

    fn fixture_db() -> (tempfile::TempDir, PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("jlcpcb.db");
        let conn = rusqlite::Connection::open(&path).unwrap();
        // The table `build_konnect_jlcpcb_database` creates.
        conn.execute_batch(
            "CREATE TABLE components (
                 LCSC TEXT NOT NULL PRIMARY KEY, MFR_Part TEXT NOT NULL,
                 Package TEXT NOT NULL, Manufacturer TEXT NOT NULL,
                 Library_Type TEXT NOT NULL, Description TEXT NOT NULL,
                 Datasheet TEXT NOT NULL, Price REAL NOT NULL,
                 Stock INTEGER NOT NULL, Category TEXT NOT NULL
             );",
        )
        .unwrap();
        let mut lines = FIXTURE.lines();
        assert_eq!(
            lines.next().unwrap(),
            "LCSC\tMFR_Part\tPackage\tManufacturer\tLibrary_Type\tDescription\tDatasheet\tPrice\tStock\tCategory"
        );
        for line in lines.filter(|line| !line.is_empty()) {
            let f: Vec<&str> = line.split('\t').collect();
            assert_eq!(f.len(), 10, "{line}");
            conn.execute(
                "INSERT INTO components VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
                rusqlite::params![
                    f[0],
                    f[1],
                    f[2],
                    f[3],
                    f[4],
                    f[5],
                    f[6],
                    f[7].parse::<f64>().unwrap(),
                    f[8].parse::<i64>().unwrap(),
                    f[9]
                ],
            )
            .unwrap();
        }
        (dir, path)
    }

    async fn served(db: &Path, arguments: serde_json::Value) -> (bool, serde_json::Value) {
        let handler = crate::mcp::handler::McpHandler::new(ServerConfig {
            kicad_cli: String::new(),
            kicad_binary: String::new(),
            ipc_address: String::new(),
            project_dir: None,
            jlcpcb_db_path: Some(db.to_path_buf()),
            auto_load_toolsets: false,
            eager_toolsets: true,
        })
        .await
        .expect("handler builds");
        let response = handler
            .handle_message(json!({
                "jsonrpc": "2.0",
                "id": 1,
                "method": "tools/call",
                "params": { "name": "suggest_jlcpcb_alternatives", "arguments": arguments }
            }))
            .await
            .expect("tools/call receives a response");
        let result = response.result.expect("result");
        let body = serde_json::from_str(result["content"][0]["text"].as_str().unwrap()).unwrap();
        (result["isError"] == json!(true), body)
    }

    fn lcsc(body: &serde_json::Value) -> Vec<&str> {
        body["alternatives"]
            .as_array()
            .unwrap_or_else(|| panic!("no alternatives: {body}"))
            .iter()
            .map(|part| part["lcsc"].as_str().unwrap())
            .collect()
    }

    /// #785: real KiCad 10 footprint IDs reach real parts, the value matches
    /// as a whole value, and the Basic part with millions in stock comes
    /// first. On `main` every one of these returned `[]`.
    #[tokio::test]
    async fn standard_kicad_footprints_find_the_value_with_the_basic_part_first() {
        let (_dir, db) = fixture_db();
        for (value, footprint, expected) in [
            (
                "100nF",
                "Capacitor_SMD:C_0402_1005Metric",
                vec!["C1525", "C307331", "C285038", "C359190", "C285045"],
            ),
            (
                "10k",
                "Resistor_SMD:R_0402_1005Metric",
                vec!["C25744", "C22356213", "C174175"],
            ),
            (
                "20pF",
                "Capacitor_SMD:C_0402_1005Metric",
                vec!["C1554", "C107000"],
            ),
            (
                "AMS1117-3.3",
                "Package_TO_SOT_SMD:SOT-223-3_TabPin2",
                vec!["C6186", "C2992570"],
            ),
            (
                "STM32F411CEU6",
                "Package_DFN_QFN:QFN-48-1EP_7x7mm_P0.5mm_EP5.6x5.6mm",
                vec!["C60420"],
            ),
            (
                "LM358",
                "Package_SO:SOIC-8_3.9x4.9mm_P1.27mm",
                vec!["C7950", "C5252902", "C5423"],
            ),
        ] {
            let (is_error, body) =
                served(&db, json!({ "value": value, "footprint": footprint })).await;
            assert!(!is_error, "{value} / {footprint}: {body}");
            assert_eq!(lcsc(&body), expected, "{value} / {footprint}: {body}");
            assert_eq!(body["count"], expected.len(), "{body}");
        }
    }

    /// The evidence for the first case: which packages were searched, how
    /// the value was matched, the ranking, and what the stock floor removed
    /// (four parts with 8 to 19 in stock, which used to lead the list).
    #[tokio::test]
    async fn the_response_states_the_package_match_ranking_and_exclusions() {
        let (_dir, db) = fixture_db();
        let (_, body) = served(
            &db,
            json!({ "value": "100nF", "footprint": "Capacitor_SMD:C_0402_1005Metric" }),
        )
        .await;
        assert_eq!(body["package_match"]["rule"], "chip_imperial");
        assert_eq!(body["package_match"]["lcsc_packages"], json!(["0402"]));
        assert_eq!(
            body["value_match"]["columns"],
            json!(["Description", "MFR_Part"])
        );
        assert_eq!(
            body["ranking"]["criteria"],
            json!([
                "library_type: Basic, Preferred, Extended",
                "known price before unknown price",
                "price ascending",
                "stock descending",
                "LCSC number"
            ])
        );
        assert_eq!(body["ranking"]["min_stock_count"], 100);
        assert_eq!(body["ranking"]["prefer_basic"], true);
        assert_eq!(body["count"], 5);
        assert_eq!(body["matched_count"], 9);
        assert_eq!(
            body["exclusions"],
            json!({
                "unknown_stock_count": 0,
                "below_min_stock_count": 4,
                "unknown_price_count": 0,
                "above_max_price_count": 0
            })
        );
    }

    /// `10k` used to match `110kΩ` and `510kΩ`, and `0402` used to match the
    /// `0402x4` resistor arrays. Neither is counted as a match now.
    #[tokio::test]
    async fn neighbouring_values_and_array_packages_are_not_matches() {
        let (_dir, db) = fixture_db();
        let (_, body) = served(
            &db,
            json!({
                "value": "10k",
                "footprint": "Resistor_SMD:R_0402_1005Metric",
                "min_stock_count": 0,
                "limit": 50
            }),
        )
        .await;
        assert_eq!(lcsc(&body), ["C25744", "C22356213", "C174175"], "{body}");
        assert_eq!(body["matched_count"], 3, "{body}");
        for part in body["alternatives"].as_array().unwrap() {
            assert_eq!(part["package"], "0402", "{part}");
            assert!(
                part["description"].as_str().unwrap().contains(" 10kΩ "),
                "{part}"
            );
        }
    }

    /// Preferred parts rank between Basic and Extended; turning the
    /// preference off ranks by price.
    #[tokio::test]
    async fn library_type_ranks_before_price_unless_turned_off() {
        let (_dir, db) = fixture_db();
        let args = |prefer_basic: bool| {
            json!({
                "value": "510k",
                "footprint": "Resistor_SMD:R_0402_1005Metric",
                "prefer_basic": prefer_basic
            })
        };
        let (_, preferred) = served(&db, args(true)).await;
        assert_eq!(lcsc(&preferred), ["C11616", "C25564"], "{preferred}");
        // C170418 has 6 in stock and an unknown price; the floor removes it.
        assert_eq!(preferred["exclusions"]["below_min_stock_count"], 1);

        let (_, by_price) = served(&db, args(false)).await;
        assert_eq!(lcsc(&by_price), ["C25564", "C11616"], "{by_price}");
        assert_eq!(by_price["ranking"]["prefer_basic"], false);
        assert_eq!(
            by_price["ranking"]["criteria"][0],
            "known price before unknown price"
        );
    }

    /// #582's stored zero is an unknown price: it sorts after every known
    /// price, and a price limit excludes it rather than admitting it as free.
    #[tokio::test]
    async fn an_unknown_price_never_sorts_as_the_cheapest() {
        let (_dir, db) = fixture_db();
        let (_, body) = served(
            &db,
            json!({ "value": "1.5M", "footprint": "Resistor_SMD:R_0402_1005Metric" }),
        )
        .await;
        assert_eq!(lcsc(&body), ["C22369344", "C138034", "C11812"], "{body}");

        let (_, limited) = served(
            &db,
            json!({
                "value": "1.5M",
                "footprint": "Resistor_SMD:R_0402_1005Metric",
                "max_price_usd": 0.0015
            }),
        )
        .await;
        assert_eq!(lcsc(&limited), ["C22369344"], "{limited}");
        assert_eq!(limited["exclusions"]["unknown_price_count"], 1, "{limited}");
        assert_eq!(
            limited["exclusions"]["above_max_price_count"], 1,
            "{limited}"
        );
    }

    /// With both overrides relaxed, the old cheapest-first order comes back,
    /// so a caller who wants it can still have it.
    #[tokio::test]
    async fn the_stock_floor_and_preference_are_overridable() {
        let (_dir, db) = fixture_db();
        let (_, body) = served(
            &db,
            json!({
                "value": "100nF",
                "footprint": "0402",
                "min_stock_count": 0,
                "prefer_basic": false
            }),
        )
        .await;
        assert_eq!(
            lcsc(&body),
            ["C285038", "C2932181", "C2838746", "C3152530", "C18255880"],
            "{body}"
        );
        assert_eq!(body["package_match"]["rule"], "lcsc_name");
        assert_eq!(body["exclusions"]["below_min_stock_count"], 0);
    }

    /// A library footprint the table does not cover is refused, naming the
    /// argument, instead of returning an empty list that reads as "no
    /// alternatives exist".
    #[tokio::test]
    async fn an_unmapped_footprint_and_an_empty_value_are_refused() {
        let (_dir, db) = fixture_db();
        let (is_error, body) = served(
            &db,
            json!({
                "value": "100nF",
                "footprint": "Connector_PinHeader_2.54mm:PinHeader_1x02_P2.54mm_Vertical"
            }),
        )
        .await;
        assert!(is_error, "{body}");
        assert_eq!(body["error"]["kind"], "invalid_argument");
        assert_eq!(body["error"]["field"], "footprint");

        let (is_error, body) = served(
            &db,
            json!({ "value": "  ", "footprint": "Capacitor_SMD:C_0402_1005Metric" }),
        )
        .await;
        assert!(is_error, "{body}");
        assert_eq!(body["error"]["field"], "value");
    }
}

#[cfg(test)]
mod catalog_unknown_values_tests {
    //! Issue #63: an unknown price or stock was stored and reported as 0, an
    //! unknown text field as "", and an unreadable row was dropped.

    use super::*;
    use crate::mcp::handler::McpHandler;
    use crate::tools::ServerConfig;

    /// A catalog of 10k 0402 resistors: a known part, a text price, an
    /// unknown stock, an unknown MPN, and an unknown price. `Price` has no
    /// declared type, as in a hand-made catalog, so a text price stays text;
    /// a `REAL` column would convert it.
    fn seed_current_catalog(dir: &Path, schema_version: i64) -> PathBuf {
        let db_path = dir.join("jlcpcb.db");
        let conn = rusqlite::Connection::open(&db_path).unwrap();
        conn.execute_batch(
            "CREATE TABLE components (LCSC TEXT, MFR_Part TEXT, Package TEXT, Manufacturer TEXT, \
             Library_Type TEXT, Description TEXT, Datasheet TEXT, Price, Stock INTEGER, \
             Category TEXT, Price_Breaks TEXT);",
        )
        .unwrap();
        conn.pragma_update(None, "user_version", schema_version)
            .unwrap();
        let rows: [(
            &str,
            Option<&str>,
            &str,
            rusqlite::types::Value,
            rusqlite::types::Value,
        ); 5] = [
            (
                "C1001",
                Some("KNOWN-1"),
                "Basic",
                0.0012.into(),
                120_000.into(),
            ),
            (
                "C1002",
                Some("TEXTPRICE-1"),
                "Basic",
                "0.0034".to_string().into(),
                500.into(),
            ),
            (
                "C1003",
                Some("NOSTOCK-1"),
                "Basic",
                0.0011.into(),
                rusqlite::types::Value::Null,
            ),
            ("C1004", None, "Basic", 0.0015.into(), 800.into()),
            (
                "C1005",
                Some("NOPRICE-1"),
                "Basic",
                rusqlite::types::Value::Null,
                900.into(),
            ),
        ];
        for (lcsc, mpn, library_type, price, stock) in rows {
            conn.execute(
                "INSERT INTO components VALUES (?1, ?2, '0402', 'Acme', ?3, '10k resistor 0402', '', ?4, ?5, 'Resistors', NULL)",
                rusqlite::params![lcsc, mpn, library_type, price, stock],
            )
            .unwrap();
        }
        db_path
    }

    async fn served(db_path: &Path) -> McpHandler {
        McpHandler::new(ServerConfig {
            jlcpcb_db_path: Some(db_path.to_path_buf()),
            eager_toolsets: true,
            ..Default::default()
        })
        .await
        .unwrap()
    }

    async fn call(
        handler: &McpHandler,
        name: &str,
        arguments: serde_json::Value,
    ) -> serde_json::Value {
        let response = handler
            .handle_message(json!({
                "jsonrpc": "2.0", "id": 1, "method": "tools/call",
                "params": { "name": name, "arguments": arguments }
            }))
            .await
            .expect("tools/call receives a response");
        let result: CallToolResult =
            serde_json::from_value(response.result.expect("a result")).unwrap();
        match result.content.first() {
            Some(crate::mcp::protocol::ToolContent::Text { text }) => {
                let mut body: serde_json::Value = serde_json::from_str(text).unwrap();
                body["is_error"] = json!(result.is_error);
                body
            }
            other => panic!("expected text content, got {other:?}"),
        }
    }

    fn by_lcsc<'a>(body: &'a serde_json::Value, key: &str, lcsc: &str) -> &'a serde_json::Value {
        body[key]
            .as_array()
            .unwrap()
            .iter()
            .find(|part| part["lcsc"] == lcsc)
            .unwrap_or_else(|| panic!("{lcsc} missing: {body}"))
    }

    /// The in-stock search scans instead of reading most of the catalog
    /// through `components_stock_idx`, which measured 3× slower on the real
    /// catalog (#286's review). The import runs no `ANALYZE`, so the planner
    /// sees the same lack of statistics here as on the real catalog.
    #[test]
    fn the_in_stock_search_keeps_off_the_stock_index() {
        let dir = tempfile::tempdir().unwrap();
        let upstream_path = dir.path().join("upstream.db");
        let output_path = dir.path().join("catalog.db");
        let conn = rusqlite::Connection::open(&upstream_path).unwrap();
        conn.execute_batch(
            "CREATE VIRTUAL TABLE parts USING fts5 (\"LCSC Part\", \"First Category\", \"Second Category\", \
             \"MFR.Part\", \"Package\", \"Manufacturer\", \"Library Type\", \"Description\", \"Datasheet\", \
             \"Price\", \"Stock\");
             INSERT INTO parts VALUES ('C1', 'R', 'Chip', 'M1', '0402', 'Acme', 'Basic', 'd', '', '0.01', '5000');",
        )
        .unwrap();
        drop(conn);
        build_konnect_jlcpcb_database(&upstream_path, &output_path).unwrap();
        let conn = rusqlite::Connection::open(&output_path).unwrap();
        let plan = |sql: &str| -> String {
            conn.prepare(&format!("EXPLAIN QUERY PLAN {sql}"))
                .unwrap()
                .query_map([], |row| row.get::<_, String>(3))
                .unwrap()
                .collect::<rusqlite::Result<Vec<_>>>()
                .unwrap()
                .join("; ")
        };
        let filters = "FROM components WHERE 1=1 AND (Description LIKE '%d%')";
        for order_by in [jlcpcb_ranking_order_by(), String::new()] {
            let search = plan(&format!(
                "SELECT LCSC {filters}{JLCPCB_IN_STOCK_FILTER}{order_by}"
            ));
            assert!(!search.contains("components_stock_idx"), "{search}");
        }
        let unknown_count = plan(&format!("SELECT COUNT(*) {filters} AND Stock IS NULL"));
        assert!(
            unknown_count.contains("components_stock_idx"),
            "{unknown_count}"
        );
    }

    /// The feed's empty and non-numeric prices and stock survive the import
    /// as NULL, and its price breaks are kept.
    #[test]
    fn import_keeps_unknown_price_and_stock_as_null() {
        let dir = tempfile::tempdir().unwrap();
        let upstream_path = dir.path().join("upstream.db");
        let output_path = dir.path().join("catalog.db");
        let conn = rusqlite::Connection::open(&upstream_path).unwrap();
        conn.execute_batch(
            "CREATE VIRTUAL TABLE parts USING fts5 (\"LCSC Part\", \"First Category\", \"Second Category\", \
             \"MFR.Part\", \"Package\", \"Manufacturer\", \"Library Type\", \"Description\", \"Datasheet\", \
             \"Price\", \"Stock\");
             INSERT INTO parts VALUES ('C1', 'R', 'Chip', 'M1', '0402', 'Acme', 'Basic', 'd', '', '1-9:0.0123,10-99:0.0110', '5000');
             INSERT INTO parts VALUES ('C2', 'R', 'Chip', 'M2', '0402', 'Acme', 'Basic', 'd', '', '', '');
             INSERT INTO parts VALUES ('C3', 'R', 'Chip', 'M3', '0402', 'Acme', 'Basic', 'd', '', 'n/a', 'unknown');
             INSERT INTO parts VALUES ('C4', 'R', 'Chip', 'M4', '0402', 'Acme', 'Basic', 'd', '', '0.5', '0');
             INSERT INTO parts VALUES ('C5', 'R', 'Chip', 'M5', '0402', 'Acme', 'Basic', 'd', '', '20+:1.5', '7.0');",
        )
        .unwrap();
        drop(conn);

        assert_eq!(
            build_konnect_jlcpcb_database(&upstream_path, &output_path).unwrap(),
            5
        );
        let conn = rusqlite::Connection::open(&output_path).unwrap();
        assert_eq!(
            catalog_schema_version(&conn).unwrap(),
            JLCPCB_CATALOG_SCHEMA_VERSION
        );
        let mut stmt = conn
            .prepare("SELECT LCSC, Price, Stock, Price_Breaks FROM components ORDER BY LCSC")
            .unwrap();
        // LCSC, Price, Stock, Price_Breaks.
        type StoredRow = (String, Option<f64>, Option<i64>, Option<String>);
        let rows: Vec<StoredRow> = stmt
            .query_map([], |row| {
                Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?))
            })
            .unwrap()
            .collect::<rusqlite::Result<_>>()
            .unwrap();
        assert_eq!(
            rows,
            [
                (
                    "C1".into(),
                    Some(0.0123),
                    Some(5000),
                    Some("1-9:0.0123,10-99:0.0110".into())
                ),
                ("C2".into(), None, None, None),
                ("C3".into(), None, None, Some("n/a".into())),
                // A real zero stays a zero.
                ("C4".into(), Some(0.5), Some(0), Some("0.5".into())),
                ("C5".into(), Some(1.5), Some(7), Some("20+:1.5".into())),
            ]
        );
    }

    /// Through served dispatch: a text price is read as its number, unknown
    /// stock and an unknown MPN are null, and the default in-stock search
    /// leaves the unknown-stock part out but counts it.
    #[tokio::test]
    async fn search_and_lookup_report_unknown_values_as_null() {
        let dir = tempfile::tempdir().unwrap();
        let handler = served(&seed_current_catalog(
            dir.path(),
            JLCPCB_CATALOG_SCHEMA_VERSION,
        ))
        .await;

        let all = call(
            &handler,
            "search_jlcpcb_parts",
            json!({ "query": "10k", "in_stock": false }),
        )
        .await;
        assert_eq!(all["count"], 5, "{all}");
        assert_eq!(by_lcsc(&all, "results", "C1002")["price"], json!(0.0034));
        let stored: String = rusqlite::Connection::open(dir.path().join("jlcpcb.db"))
            .unwrap()
            .query_row(
                "SELECT typeof(Price) FROM components WHERE LCSC = 'C1002'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(stored, "text", "the fixture must hold a text price");
        assert_eq!(by_lcsc(&all, "results", "C1003")["stock"], json!(null));
        assert_eq!(by_lcsc(&all, "results", "C1004")["mpn"], json!(null));
        assert_eq!(by_lcsc(&all, "results", "C1005")["price"], json!(null));
        assert_eq!(by_lcsc(&all, "results", "C1001")["price"], json!(0.0012));
        assert!(all.get("unknown_stock_excluded").is_none(), "{all}");
        assert!(all.get("catalog_note").is_none(), "{all}");

        let stocked = call(&handler, "search_jlcpcb_parts", json!({ "query": "10k" })).await;
        assert_eq!(stocked["count"], 4, "{stocked}");
        assert_eq!(stocked["unknown_stock_excluded"], 1);

        let part = call(&handler, "get_jlcpcb_part", json!({ "lcsc_id": "C1003" })).await;
        assert_eq!(part["stock"], json!(null), "{part}");
        assert_eq!(part["price"], json!(0.0011));
        let part = call(&handler, "get_jlcpcb_part", json!({ "lcsc_id": "C1004" })).await;
        assert_eq!(part["mpn"], json!(null), "{part}");
    }

    /// An unknown price is not the cheapest alternative, and an unknown stock
    /// is counted on its own rather than as below the stock floor.
    #[tokio::test]
    async fn alternatives_rank_unknown_price_last_and_count_unknown_stock() {
        let dir = tempfile::tempdir().unwrap();
        let handler = served(&seed_current_catalog(
            dir.path(),
            JLCPCB_CATALOG_SCHEMA_VERSION,
        ))
        .await;
        let body = call(
            &handler,
            "suggest_jlcpcb_alternatives",
            json!({ "value": "10k", "footprint": "Resistor_SMD:R_0402_1005Metric", "min_stock_count": 1 }),
        )
        .await;
        let order: Vec<&str> = body["alternatives"]
            .as_array()
            .unwrap()
            .iter()
            .map(|part| part["lcsc"].as_str().unwrap())
            .collect();
        assert_eq!(order, ["C1001", "C1004", "C1002", "C1005"], "{body}");
        assert_eq!(body["exclusions"]["unknown_stock_count"], 1);
        assert_eq!(body["exclusions"]["below_min_stock_count"], 0);
    }

    /// A catalog from before #63 is reported, and its zeros are not turned
    /// into unknowns.
    #[tokio::test]
    async fn an_old_catalog_is_reported_and_its_zeros_kept() {
        let dir = tempfile::tempdir().unwrap();
        let db_path = seed_current_catalog(dir.path(), 1);
        rusqlite::Connection::open(&db_path)
            .unwrap()
            .execute(
                "UPDATE components SET Price = 0, Stock = 0 WHERE LCSC = 'C1005'",
                [],
            )
            .unwrap();
        let handler = served(&db_path).await;

        let search = call(
            &handler,
            "search_jlcpcb_parts",
            json!({ "query": "10k", "in_stock": false }),
        )
        .await;
        assert!(
            search["catalog_note"]
                .as_str()
                .unwrap()
                .contains("force: true"),
            "{search}"
        );
        assert_eq!(by_lcsc(&search, "results", "C1005")["price"], json!(0.0));
        assert_eq!(by_lcsc(&search, "results", "C1005")["stock"], json!(0));
        // An old catalog cannot say which zeros are unknown, so it counts none.
        let stocked = call(&handler, "search_jlcpcb_parts", json!({ "query": "10k" })).await;
        assert!(stocked.get("unknown_stock_excluded").is_none(), "{stocked}");

        for (tool, arguments) in [
            ("get_jlcpcb_part", json!({ "lcsc_id": "C1001" })),
            ("get_jlcpcb_database_stats", json!({})),
            (
                "suggest_jlcpcb_alternatives",
                json!({ "value": "10k", "footprint": "Resistor_SMD:R_0402_1005Metric" }),
            ),
        ] {
            let body = call(&handler, tool, arguments).await;
            assert!(body["catalog_note"].is_string(), "{tool}: {body}");
        }
    }

    #[tokio::test]
    async fn stats_report_the_schema_version_and_unknown_counts() {
        let dir = tempfile::tempdir().unwrap();
        let handler = served(&seed_current_catalog(
            dir.path(),
            JLCPCB_CATALOG_SCHEMA_VERSION,
        ))
        .await;
        let stats = call(&handler, "get_jlcpcb_database_stats", json!({})).await;
        assert_eq!(
            stats["schema_version"], JLCPCB_CATALOG_SCHEMA_VERSION,
            "{stats}"
        );
        assert_eq!(stats["unknown_price_count"], 1);
        assert_eq!(stats["unknown_stock_count"], 1);
        assert!(stats.get("catalog_note").is_none());
    }

    /// A row that cannot be read is an error, not a missing result.
    #[tokio::test]
    async fn an_unreadable_row_is_an_error_not_a_missing_result() {
        let dir = tempfile::tempdir().unwrap();
        let db_path = seed_current_catalog(dir.path(), JLCPCB_CATALOG_SCHEMA_VERSION);
        rusqlite::Connection::open(&db_path)
            .unwrap()
            .execute(
                "INSERT INTO components VALUES (NULL, 'ORPHAN-1', '0402', 'Acme', 'Basic', '10k resistor 0402', '', 0.1, 10, 'Resistors', NULL)",
                [],
            )
            .unwrap();
        let handler = served(&db_path).await;
        let body = call(&handler, "search_jlcpcb_parts", json!({ "query": "10k" })).await;
        assert_eq!(body["is_error"], true, "{body}");
        assert!(
            body["message"]
                .as_str()
                .unwrap()
                .contains("has no LCSC number"),
            "{body}"
        );
    }
}
