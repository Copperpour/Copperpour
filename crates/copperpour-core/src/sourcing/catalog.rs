//! The local JLCPCB parts catalog (#209): where it is, how it is built from
//! the feed, and how its rows are read and ranked. The `integration` tools'
//! handlers call it.

pub(crate) use super::parts::{CatalogPrice, CatalogPrices, JlcpchPartInfo};
use crate::sourcing::lcsc_api::get_with_backoff;
use crate::tools::{lcsc_packages, ToolContext};
use serde_json::json;
use std::collections::{HashMap, HashSet};
use std::io::{self, BufWriter};
use std::path::{Path, PathBuf};
use tokio::io::AsyncWriteExt;

pub(crate) const JLCPCB_FEED_BASE_URL: &str = "https://bouni.github.io/kicad-jlcpcb-tools";

pub(crate) const JLCPCB_FEED_SENTINEL: &str = "chunk_num_current_parts_fts5.txt";

pub(crate) const JLCPCB_FEED_ARCHIVE: &str = "current-parts-fts5.db.zip";

pub(crate) const JLCPCB_MAX_CHUNKS: usize = 64;

pub(crate) const JLCPCB_MAX_ARCHIVE_BYTES: u64 = 1_073_741_824;

pub(crate) const JLCPCB_MAX_DATABASE_BYTES: u64 = 8_589_934_592;

pub(crate) fn default_jlcpcb_db_path() -> Option<PathBuf> {
    crate::app_dirs::user_data_dir().map(|dir| dir.join("jlcpcb.db"))
}

/// `None` when no path is given and the user's home directory is unknown.
///
/// Both names are read because the tools sharing this disagree:
/// `enrich_datasheets` declares `db_path`, `download_jlcpcb_database` declares
/// `output_path`, and the other callers declare neither and use the configured
/// or default catalog. Each schema refuses the names it does not declare, so
/// through served dispatch a tool only ever passes its own.
pub(crate) fn resolve_db_path(args: &serde_json::Value, ctx: &ToolContext) -> Option<PathBuf> {
    if let Some(p) = args["db_path"]
        .as_str()
        .or_else(|| args["output_path"].as_str())
    {
        return Some(PathBuf::from(p));
    }
    if let Some(p) = &ctx.config.jlcpcb_db_path {
        return Some(p.clone());
    }
    default_jlcpcb_db_path()
}

/// Prices, price breaks and library types of the given LCSC IDs in the local
/// catalog, opened read-only. A price that is not a number reads as unknown,
/// and price breaks that are not text as none.
pub(crate) fn query_catalog_prices(
    db_path: &Path,
    lcsc_ids: &[String],
) -> anyhow::Result<CatalogPrices> {
    let conn =
        rusqlite::Connection::open_with_flags(db_path, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)?;
    let has_price_breaks = catalog_schema_version(&conn)? >= JLCPCB_CATALOG_SCHEMA_VERSION;
    let mut stmt = conn.prepare(if has_price_breaks {
        "SELECT Price, Library_Type, Price_Breaks FROM components WHERE LCSC = ?1 LIMIT 1"
    } else {
        "SELECT Price, Library_Type, NULL FROM components WHERE LCSC = ?1 LIMIT 1"
    })?;
    let mut prices = CatalogPrices {
        has_price_breaks,
        ..CatalogPrices::default()
    };
    for id in lcsc_ids {
        let mut rows = stmt.query_map(rusqlite::params![id], |row| {
            Ok(CatalogPrice {
                price: row.get::<_, Option<f64>>(0).ok().flatten().filter(|price| {
                    price.is_finite() && (*price > 0.0 || has_price_breaks && *price == 0.0)
                }),
                library_type: row.get::<_, String>(1).unwrap_or_default(),
                price_breaks: row.get::<_, String>(2).ok(),
            })
        })?;
        if let Some(price) = rows.next().transpose()? {
            prices.parts.insert(id.clone(), price);
        }
    }
    Ok(prices)
}

/// The part with this LCSC number, or `None` when the catalog doesn't have
/// it. A field the row doesn't hold as text reads as empty.
pub(crate) fn query_part_info(
    db_path: &Path,
    lcsc_id: &str,
) -> anyhow::Result<Option<JlcpchPartInfo>> {
    let conn =
        rusqlite::Connection::open_with_flags(db_path, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)?;
    let mut stmt = conn.prepare(
        "SELECT LCSC, MFR_Part, Manufacturer, Datasheet, Description FROM components WHERE LCSC = ?1 LIMIT 1",
    )?;
    let mut rows = stmt.query_map(rusqlite::params![lcsc_id], |row| {
        Ok(JlcpchPartInfo {
            lcsc: row.get::<_, String>(0).unwrap_or_default(),
            mfr_part: row.get::<_, String>(1).unwrap_or_default(),
            manufacturer: row.get::<_, String>(2).unwrap_or_default(),
            datasheet: row.get::<_, String>(3).unwrap_or_default(),
            description: row.get::<_, String>(4).unwrap_or_default(),
        })
    })?;
    Ok(rows.next().transpose()?)
}

/// The datasheet of the part with this LCSC number, or else this MPN. `None`
/// when the catalog doesn't have the part, or has no datasheet for it.
pub(crate) fn query_datasheet(
    db_path: &Path,
    lcsc_id: Option<&str>,
    mpn: Option<&str>,
) -> anyhow::Result<Option<String>> {
    let (sql, param) = match (lcsc_id, mpn) {
        // An LCSC ID is the primary key shape, so it wins when both are given.
        (Some(id), _) => (
            "SELECT Datasheet FROM components WHERE LCSC = ?1 LIMIT 1",
            id,
        ),
        (None, Some(mpn)) => (
            "SELECT Datasheet FROM components WHERE MFR_Part = ?1 LIMIT 1",
            mpn,
        ),
        (None, None) => return Ok(None),
    };
    let conn =
        rusqlite::Connection::open_with_flags(db_path, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)?;
    let mut stmt = conn.prepare(sql)?;
    let mut rows = stmt.query_map(rusqlite::params![param], |row| catalog_text(row, 0))?;
    Ok(rows
        .next()
        .transpose()?
        .flatten()
        .filter(|url| !url.is_empty()))
}

pub(crate) fn parse_jlcpcb_chunk_count(value: &str) -> anyhow::Result<usize> {
    let chunk_count = value
        .trim()
        .parse::<usize>()
        .map_err(|_| anyhow::anyhow!("Invalid JLCPCB feed chunk count: {value:?}"))?;
    if !(1..=JLCPCB_MAX_CHUNKS).contains(&chunk_count) {
        anyhow::bail!(
            "JLCPCB feed chunk count {chunk_count} is outside the supported range 1..={JLCPCB_MAX_CHUNKS}"
        );
    }
    Ok(chunk_count)
}

pub(crate) async fn download_jlcpcb_archive(
    client: &reqwest::Client,
    archive_path: &Path,
) -> anyhow::Result<(usize, u64)> {
    let sentinel_url = format!("{JLCPCB_FEED_BASE_URL}/{JLCPCB_FEED_SENTINEL}");
    let sentinel_response = get_with_backoff(client, &sentinel_url).await?;
    if !sentinel_response.status().is_success() {
        anyhow::bail!(
            "Failed to read JLCPCB feed manifest: HTTP {}",
            sentinel_response.status()
        );
    }
    let chunk_count = parse_jlcpcb_chunk_count(&sentinel_response.text().await?)?;

    let mut archive = tokio::fs::File::create(archive_path).await?;
    let mut downloaded_bytes = 0u64;
    for chunk_number in 1..=chunk_count {
        let chunk_url = format!("{JLCPCB_FEED_BASE_URL}/{JLCPCB_FEED_ARCHIVE}.{chunk_number:03}");
        let mut response = get_with_backoff(client, &chunk_url).await?;
        if !response.status().is_success() {
            anyhow::bail!(
                "Failed to download JLCPCB database chunk {chunk_number}/{chunk_count}: HTTP {}",
                response.status()
            );
        }

        while let Some(bytes) = response.chunk().await? {
            downloaded_bytes = downloaded_bytes
                .checked_add(bytes.len() as u64)
                .ok_or_else(|| anyhow::anyhow!("JLCPCB archive size overflow"))?;
            if downloaded_bytes > JLCPCB_MAX_ARCHIVE_BYTES {
                anyhow::bail!(
                    "JLCPCB archive exceeds the {} byte safety limit",
                    JLCPCB_MAX_ARCHIVE_BYTES
                );
            }
            archive.write_all(&bytes).await?;
        }
    }
    archive.sync_all().await?;
    Ok((chunk_count, downloaded_bytes))
}

pub(crate) fn extract_jlcpcb_database(
    archive_path: &Path,
    output_path: &Path,
) -> anyhow::Result<()> {
    let archive_file = std::fs::File::open(archive_path)?;
    let mut archive = zip::ZipArchive::new(archive_file)?;
    if archive.len() != 1 {
        anyhow::bail!(
            "JLCPCB archive must contain exactly one database file; found {} entries",
            archive.len()
        );
    }

    let mut database = archive.by_index(0)?;
    if database.is_dir() || database.enclosed_name().is_none() {
        anyhow::bail!("JLCPCB archive does not contain a safe database file");
    }
    if database.size() > JLCPCB_MAX_DATABASE_BYTES {
        anyhow::bail!(
            "JLCPCB database exceeds the {} byte safety limit",
            JLCPCB_MAX_DATABASE_BYTES
        );
    }

    let output_file = std::fs::File::create(output_path)?;
    let mut output = BufWriter::new(output_file);
    io::copy(&mut database, &mut output)?;
    output.into_inner()?.sync_all()?;
    Ok(())
}

pub(crate) fn validate_upstream_schema(conn: &rusqlite::Connection) -> anyhow::Result<()> {
    let mut statement = conn.prepare("PRAGMA table_info(parts)")?;
    let columns: HashSet<String> = statement
        .query_map([], |row| row.get(1))?
        .collect::<rusqlite::Result<_>>()?;
    let required = [
        "LCSC Part",
        "First Category",
        "Second Category",
        "MFR.Part",
        "Package",
        "Manufacturer",
        "Library Type",
        "Description",
        "Datasheet",
        "Price",
        "Stock",
    ];
    let missing: Vec<&str> = required
        .into_iter()
        .filter(|column| !columns.contains(*column))
        .collect();
    if !missing.is_empty() {
        anyhow::bail!(
            "JLCPCB feed schema is missing required columns: {}",
            missing.join(", ")
        );
    }
    Ok(())
}

/// The `user_version` of a catalog that keeps unknown prices and stock as
/// NULL (#63). Version 1 stored them as 0, which cannot be told from a real 0.
pub(crate) const JLCPCB_CATALOG_SCHEMA_VERSION: i64 = 2;

pub(crate) fn build_konnect_jlcpcb_database(
    upstream_path: &Path,
    output_path: &Path,
) -> anyhow::Result<u64> {
    let upstream = rusqlite::Connection::open_with_flags(
        upstream_path,
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
    )?;
    validate_upstream_schema(&upstream)?;
    drop(upstream);

    let mut output = rusqlite::Connection::open(output_path)?;
    output.execute_batch(
        "PRAGMA journal_mode = OFF;
         PRAGMA synchronous = OFF;
         PRAGMA temp_store = MEMORY;",
    )?;
    let upstream_path = upstream_path.to_string_lossy().into_owned();
    output.execute(
        "ATTACH DATABASE ?1 AS upstream",
        rusqlite::params![upstream_path],
    )?;

    let transaction = output.transaction()?;
    transaction.execute_batch(
        "CREATE TABLE components (
             LCSC TEXT NOT NULL PRIMARY KEY,
             MFR_Part TEXT NOT NULL,
             Package TEXT NOT NULL,
             Manufacturer TEXT NOT NULL,
             Library_Type TEXT NOT NULL,
             Description TEXT NOT NULL,
             Datasheet TEXT NOT NULL,
             Price REAL,
             Stock INTEGER,
             Category TEXT NOT NULL,
             Price_Breaks TEXT
         );",
    )?;
    transaction.pragma_update(None, "user_version", JLCPCB_CATALOG_SCHEMA_VERSION)?;
    // A price or stock cell that is empty or not a number is stored as NULL,
    // not 0, so a reader can tell "unknown" from free or out of stock (#63).
    // The price is the first price break; all of them are kept as given in
    // Price_Breaks, e.g. `1-9:0.012,10-99:0.008`.
    let imported = transaction.execute(
        "INSERT INTO components (
             LCSC, MFR_Part, Package, Manufacturer, Library_Type,
             Description, Datasheet, Price, Stock, Category, Price_Breaks
         )
         SELECT
             COALESCE(\"LCSC Part\", ''),
             COALESCE(\"MFR.Part\", ''),
             COALESCE(\"Package\", ''),
             COALESCE(\"Manufacturer\", ''),
             COALESCE(\"Library Type\", ''),
             COALESCE(\"Description\", ''),
             COALESCE(\"Datasheet\", ''),
             CASE
                 WHEN first_price GLOB '[0-9]*' OR first_price GLOB '.[0-9]*'
                 THEN CAST(first_price AS REAL)
             END,
             CASE
                 WHEN stock_text <> '' AND stock_text NOT GLOB '*[^0-9]*'
                 THEN CAST(stock_text AS INTEGER)
                 WHEN stock_text GLOB '[0-9]*.0'
                     AND substr(stock_text, 1, length(stock_text) - 2) NOT GLOB '*[^0-9]*'
                 THEN CAST(stock_text AS INTEGER)
             END,
             CASE
                 WHEN COALESCE(\"First Category\", '') = '' THEN COALESCE(\"Second Category\", '')
                 WHEN COALESCE(\"Second Category\", '') = '' THEN \"First Category\"
                 ELSE \"First Category\" || ' / ' || \"Second Category\"
             END,
             NULLIF(price_text, '')
         FROM (
             SELECT *,
                 trim(CASE
                     WHEN instr(price_text, ':') > 0
                     THEN substr(price_text, instr(price_text, ':') + 1)
                     ELSE price_text
                 END) AS first_price
             FROM (
                 SELECT *,
                     trim(CAST(COALESCE(\"Price\", '') AS TEXT)) AS price_text,
                     trim(CAST(COALESCE(\"Stock\", '') AS TEXT)) AS stock_text
                 FROM upstream.parts
             )
         )",
        [],
    )?;
    // components_stock_idx lets the in-stock search count the parts whose
    // stock is unknown (`Stock IS NULL`) without scanning the table.
    transaction.execute_batch(
        "CREATE INDEX components_mfr_part_idx ON components(MFR_Part);
         CREATE INDEX components_package_idx ON components(Package);
         CREATE INDEX components_library_stock_idx ON components(Library_Type, Stock);
         CREATE INDEX components_category_idx ON components(Category);
         CREATE INDEX components_stock_idx ON components(Stock);",
    )?;
    transaction.commit()?;
    output.execute_batch("DETACH DATABASE upstream; PRAGMA synchronous = FULL;")?;

    let part_count: i64 =
        output.query_row("SELECT COUNT(*) FROM components", [], |row| row.get(0))?;
    if imported == 0 || part_count <= 0 {
        anyhow::bail!("JLCPCB feed did not contain any components");
    }
    let quick_check: String = output.query_row("PRAGMA quick_check", [], |row| row.get(0))?;
    if quick_check != "ok" {
        anyhow::bail!("Generated JLCPCB database failed validation: {quick_check}");
    }
    drop(output);
    Ok(part_count.try_into()?)
}

pub(crate) fn replace_jlcpcb_database(
    staged_path: &Path,
    destination: &Path,
    backup_path: &Path,
) -> anyhow::Result<()> {
    if !destination.exists() {
        std::fs::rename(staged_path, destination)?;
        return Ok(());
    }

    std::fs::rename(destination, backup_path)?;
    if let Err(install_error) = std::fs::rename(staged_path, destination) {
        if let Err(restore_error) = std::fs::rename(backup_path, destination) {
            anyhow::bail!(
                "Failed to install the new JLCPCB database ({install_error}) and restore the previous database ({restore_error})"
            );
        }
        return Err(install_error.into());
    }
    std::fs::remove_file(backup_path)?;
    Ok(())
}

/// Columns `search_jlcpcb_parts` matches every query word against, in the
/// order they appear in the SQL and in the response's `search_fields`.
pub(crate) const JLCPCB_SEARCH_FIELDS: [&str; 5] =
    ["Description", "MFR_Part", "LCSC", "Package", "Manufacturer"];

/// The query words a part must all contain: the single source for both the
/// SQL and the `tokens` evidence in the response.
pub(crate) fn jlcpcb_search_tokens(query: &str) -> Vec<String> {
    query.split_whitespace().map(String::from).collect()
}

/// Whether a query word reads as a component value: a number, optionally
/// followed by an SI prefix and a unit (`20pF`, `20p`, `4.7uF`, `1k`, `10kΩ`,
/// `16V`, `8MHz`, `0402`). Such a word must match as a whole value, so `20pF`
/// no longer finds `220pF` and `1k` no longer finds `10k` (#786). Any other
/// word, an MPN prefix like `0603WAF` included, still matches as a substring.
pub(crate) fn is_jlcpcb_value_token(token: &str) -> bool {
    let lower = token.to_lowercase();
    let rest = lower.trim_start_matches(|c: char| c.is_ascii_digit() || c == '.');
    if rest.len() == lower.len() || !lower.starts_with(|c: char| c.is_ascii_digit()) {
        return false;
    }
    let rest = rest
        .strip_prefix(['p', 'n', 'u', 'µ', 'm', 'k', 'g'])
        .unwrap_or(rest);
    matches!(
        rest,
        "" | "f" | "h" | "v" | "a" | "w" | "r" | "ω" | "ohm" | "hz"
    )
}

/// `%`, `_` and `\` matched literally by a `LIKE … ESCAPE '\'`.
pub(crate) fn escape_like(text: &str) -> String {
    text.replace('\\', "\\\\")
        .replace('%', "\\%")
        .replace('_', "\\_")
}

/// The separator between a category's group and subcategory in the catalog,
/// as the database build writes it.
pub(crate) const JLCPCB_CATEGORY_SEPARATOR: &str = " / ";

/// A catalog text cell, or `None` when it is NULL or cannot be read as text.
/// A number is read as its text, as a hand-made catalog may store an MPN.
pub(crate) fn catalog_text(row: &rusqlite::Row, index: usize) -> rusqlite::Result<Option<String>> {
    use rusqlite::types::ValueRef;
    Ok(match row.get_ref(index)? {
        ValueRef::Text(text) => std::str::from_utf8(text).ok().map(str::to_owned),
        ValueRef::Integer(number) => Some(number.to_string()),
        ValueRef::Real(number) => Some(number.to_string()),
        ValueRef::Null | ValueRef::Blob(_) => None,
    })
}

/// A catalog price, or `None` when it is NULL or not a number (#63). A text
/// cell that holds a number is read as that number.
pub(crate) fn catalog_price(row: &rusqlite::Row, index: usize) -> rusqlite::Result<Option<f64>> {
    use rusqlite::types::ValueRef;
    Ok(match row.get_ref(index)? {
        ValueRef::Real(price) => Some(price),
        ValueRef::Integer(price) => Some(price as f64),
        ValueRef::Text(text) => std::str::from_utf8(text)
            .ok()
            .and_then(|text| text.trim().parse::<f64>().ok()),
        ValueRef::Null | ValueRef::Blob(_) => None,
    }
    .filter(|price| price.is_finite()))
}

/// A catalog stock count, or `None` when it is NULL or not a whole number.
pub(crate) fn catalog_stock(row: &rusqlite::Row, index: usize) -> rusqlite::Result<Option<i64>> {
    use rusqlite::types::ValueRef;
    Ok(match row.get_ref(index)? {
        ValueRef::Integer(stock) => Some(stock),
        ValueRef::Real(stock) if stock.is_finite() && stock.fract() == 0.0 => Some(stock as i64),
        ValueRef::Text(text) => std::str::from_utf8(text)
            .ok()
            .and_then(|text| text.trim().parse::<i64>().ok()),
        _ => None,
    })
}

/// A part as the catalog tools report it. A value the catalog does not have,
/// or cannot convert, is null: an unknown price is not free and an unknown
/// stock is not out of stock (#63). A row without an LCSC number is an error.
pub(crate) fn row_to_part_json(row: &rusqlite::Row) -> rusqlite::Result<serde_json::Value> {
    let Some(lcsc) = catalog_text(row, 0)?.filter(|lcsc| !lcsc.trim().is_empty()) else {
        return Err(rusqlite::Error::FromSqlConversionFailure(
            0,
            rusqlite::types::Type::Null,
            "a JLCPCB catalog row has no LCSC number".into(),
        ));
    };
    // The exact LCSC PDF URL for most parts. Omitted here once made a caller
    // unable to get a datasheet the table already held (#255); an empty cell
    // reads as null rather than "" so callers can test presence directly.
    let datasheet_url = catalog_text(row, 6)?.filter(|url| !url.is_empty());
    Ok(json!({
        "lcsc": lcsc,
        "mpn": catalog_text(row, 1)?,
        "package": catalog_text(row, 2)?,
        "manufacturer": catalog_text(row, 3)?,
        "library_type": catalog_text(row, 4)?,
        "description": catalog_text(row, 5)?,
        "datasheet_url": datasheet_url,
        "price": catalog_price(row, 7)?,
        "stock": catalog_stock(row, 8)?
    }))
}

/// Whether every value word matches one of the part's searchable fields as a
/// whole value. The SQL `LIKE` cannot see value boundaries.
pub(crate) fn part_has_values(part: &serde_json::Value, value_tokens: &[&String]) -> bool {
    value_tokens.iter().all(|token| {
        ["description", "mpn", "lcsc", "package", "manufacturer"]
            .iter()
            .any(|field| {
                part[*field]
                    .as_str()
                    .is_some_and(|text| lcsc_packages::contains_whole_value(text, token))
            })
    })
}

/// The parts a catalog query returns that have the value words, up to
/// `limit`. A row that cannot be read is an error, not a missing result (#63).
pub(crate) fn collect_catalog_parts(
    statement: &mut rusqlite::Statement,
    params: &[String],
    value_tokens: &[&String],
    limit: usize,
) -> rusqlite::Result<Vec<serde_json::Value>> {
    statement
        .query_map(rusqlite::params_from_iter(params.iter()), row_to_part_json)?
        .filter(|part| {
            part.as_ref()
                .map_or(true, |part| part_has_values(part, value_tokens))
        })
        .take(limit)
        .collect()
}

/// The catalog's `user_version`: 0 for a hand-made catalog, 1 for one built
/// before #63, [`JLCPCB_CATALOG_SCHEMA_VERSION`] for a current one.
pub(crate) fn catalog_schema_version(conn: &rusqlite::Connection) -> rusqlite::Result<i64> {
    conn.query_row("PRAGMA user_version", [], |row| row.get(0))
}

/// The note the catalog tools add for a catalog that stored unknown prices
/// and stock as 0. Its zeros are reported as they are, not as unknown.
pub(crate) fn old_catalog_note(schema_version: i64) -> Option<&'static str> {
    (schema_version < JLCPCB_CATALOG_SCHEMA_VERSION).then_some(
        "This JLCPCB catalog was built before unknown prices and stock were kept, so a price or \
         stock of 0 in it may mean unknown. Run download_jlcpcb_database with force: true to rebuild it.",
    )
}

/// JLCPCB library types in order of preference. Basic parts carry no setup
/// fee, Preferred parts none for economic assembly, and every unique Extended
/// part adds one.
pub(crate) const LIBRARY_TYPE_ORDER: [&str; 3] = ["Basic", "Preferred", "Extended"];

pub(crate) const SUGGEST_DEFAULT_MIN_STOCK_COUNT: u64 = 100;

/// A part that matched both the value and the package, with the columns the
/// stock floor, price limit and ranking read.
#[derive(Debug, Clone)]
pub(crate) struct AlternativeCandidate {
    pub(crate) part: serde_json::Value,
    pub(crate) lcsc_id: String,
    pub(crate) library_type: String,
    /// 0 when the price is unknown.
    pub(crate) price: f64,
    /// `None` when the catalog does not know the stock (#63).
    pub(crate) stock_count: Option<i64>,
}

impl AlternativeCandidate {
    /// A catalog from before #63 stored a price the feed did not give as 0,
    /// and no part is free, so 0 or less means the price is unknown, as NULL
    /// does in a current catalog.
    pub(crate) fn price_known(&self) -> bool {
        self.price > 0.0
    }

    pub(crate) fn library_rank(&self) -> usize {
        LIBRARY_TYPE_ORDER
            .iter()
            .position(|kind| *kind == self.library_type)
            .unwrap_or(LIBRARY_TYPE_ORDER.len())
    }
}

#[derive(Debug, Clone, Copy)]
pub(crate) struct AlternativePolicy {
    pub(crate) min_stock_count: u64,
    pub(crate) prefer_basic: bool,
    pub(crate) max_price: Option<f64>,
    pub(crate) limit: usize,
}

impl AlternativePolicy {
    pub(crate) fn criteria(&self) -> Vec<&'static str> {
        jlcpcb_ranking_criteria(self.prefer_basic)
    }
}

/// The ranking `suggest_jlcpcb_alternatives` and `search_jlcpcb_parts` share,
/// most significant first, as the responses state it.
pub(crate) fn jlcpcb_ranking_criteria(by_library_type: bool) -> Vec<&'static str> {
    let mut criteria = Vec::new();
    if by_library_type {
        criteria.push("library_type: Basic, Preferred, Extended");
    }
    criteria.extend([
        "known price before unknown price",
        "price ascending",
        "stock descending",
        "LCSC number",
    ]);
    criteria
}

/// `search_jlcpcb_parts`' ranking: in-stock parts first, then
/// [`jlcpcb_ranking_criteria`]. `suggest_jlcpcb_alternatives` needs no stock
/// tier, because it drops parts below its stock floor; a search with
/// `in_stock: false` keeps them, and they rank last.
pub(crate) fn search_ranking_criteria() -> Vec<&'static str> {
    std::iter::once("in stock before out of stock")
        .chain(jlcpcb_ranking_criteria(true))
        .collect()
}

/// The search's `in_stock` filter. Most parts are in stock, so
/// `components_stock_idx` would make it slower than a scan, and the catalog
/// has no `ANALYZE` statistics to tell SQLite so. The unary `+` keeps SQLite
/// from using the index here; the `Stock IS NULL` count still uses it.
pub(crate) const JLCPCB_IN_STOCK_FILTER: &str = " AND +Stock > 0";

/// [`search_ranking_criteria`] as an `ORDER BY`. A current catalog stores an
/// unknown price as NULL and one from before #63 as 0. Both rank as unknown
/// and sort as 0, as [`AlternativeCandidate`] reads them.
pub(crate) fn jlcpcb_ranking_order_by() -> String {
    let tiers: String = LIBRARY_TYPE_ORDER
        .iter()
        .enumerate()
        .map(|(rank, kind)| format!(" WHEN '{kind}' THEN {rank}"))
        .collect();
    format!(
        " ORDER BY Stock > 0 DESC, CASE Library_Type{tiers} ELSE {} END, COALESCE(Price, 0) > 0 DESC, COALESCE(Price, 0), Stock DESC, LCSC",
        LIBRARY_TYPE_ORDER.len()
    )
}

/// What filtering removed and what was returned, so an empty or short result
/// says why.
#[derive(Debug, Default, PartialEq, Eq)]
pub(crate) struct AlternativeCounts {
    pub(crate) matched: usize,
    pub(crate) unknown_stock: usize,
    pub(crate) below_min_stock: usize,
    pub(crate) unknown_price: usize,
    pub(crate) above_max_price: usize,
    pub(crate) returned: usize,
}

/// Apply the stock floor and price limit, then rank. Ranking by price alone
/// put Extended parts with a dozen units in stock, and parts whose price was
/// unknown, ahead of the Basic part with millions (#785).
pub(crate) fn rank_alternatives(
    candidates: Vec<AlternativeCandidate>,
    policy: &AlternativePolicy,
) -> (Vec<AlternativeCandidate>, AlternativeCounts) {
    let mut counts = AlternativeCounts {
        matched: candidates.len(),
        ..AlternativeCounts::default()
    };
    let min_stock_count = i64::try_from(policy.min_stock_count).unwrap_or(i64::MAX);
    let mut kept: Vec<AlternativeCandidate> = Vec::new();
    for candidate in candidates {
        // A stock the catalog does not know cannot meet the floor, but it is
        // not below it either (#63).
        let Some(stock_count) = candidate.stock_count else {
            counts.unknown_stock += 1;
            continue;
        };
        if stock_count < min_stock_count {
            counts.below_min_stock += 1;
        } else if let Some(max_price) = policy.max_price {
            if !candidate.price_known() {
                counts.unknown_price += 1;
            } else if candidate.price > max_price {
                counts.above_max_price += 1;
            } else {
                kept.push(candidate);
            }
        } else {
            kept.push(candidate);
        }
    }

    let tier = |candidate: &AlternativeCandidate| {
        if policy.prefer_basic {
            candidate.library_rank()
        } else {
            0
        }
    };
    kept.sort_by(|a, b| {
        tier(a)
            .cmp(&tier(b))
            .then(b.price_known().cmp(&a.price_known()))
            .then(a.price.total_cmp(&b.price))
            .then(b.stock_count.cmp(&a.stock_count))
            .then(a.lcsc_id.cmp(&b.lcsc_id))
    });
    kept.truncate(policy.limit);
    counts.returned = kept.len();
    (kept, counts)
}

/// Read every part in one of `packages` whose description or part number
/// contains `value` as a whole value. SQL narrows by substring; the
/// whole-value test, which SQLite's LIKE cannot express, runs here.
pub(crate) fn query_alternative_candidates(
    db_path: &Path,
    value: &str,
    packages: &[String],
) -> anyhow::Result<(Vec<AlternativeCandidate>, i64)> {
    let conn =
        rusqlite::Connection::open_with_flags(db_path, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)?;
    let schema_version = catalog_schema_version(&conn)?;
    let placeholders = (1..=packages.len())
        .map(|i| format!("?{i}"))
        .collect::<Vec<_>>()
        .join(", ");
    let like = packages.len() + 1;
    let sql = format!(
        "SELECT LCSC, MFR_Part, Package, Manufacturer, Library_Type, Description, Datasheet, Price, Stock \
         FROM components WHERE Package COLLATE NOCASE IN ({placeholders}) \
         AND (Description LIKE ?{like} ESCAPE '\\' OR MFR_Part LIKE ?{like} ESCAPE '\\')"
    );
    let mut params: Vec<String> = packages.to_vec();
    params.push(format!("%{}%", escape_like(value)));

    let mut statement = conn.prepare(&sql)?;
    let rows = statement
        .query_map(rusqlite::params_from_iter(params.iter()), |row| {
            let mpn: String = catalog_text(row, 1)?.unwrap_or_default();
            let description: String = catalog_text(row, 5)?.unwrap_or_default();
            Ok((
                mpn,
                description,
                AlternativeCandidate {
                    part: row_to_part_json(row)?,
                    lcsc_id: catalog_text(row, 0)?.unwrap_or_default(),
                    library_type: catalog_text(row, 4)?.unwrap_or_default(),
                    price: catalog_price(row, 7)?.unwrap_or(0.0),
                    stock_count: catalog_stock(row, 8)?,
                },
            ))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    let candidates = rows
        .into_iter()
        .filter(|(mpn, description, _)| {
            lcsc_packages::contains_whole_value(description, value)
                || lcsc_packages::contains_whole_value(mpn, value)
        })
        .map(|(_, _, candidate)| candidate)
        .collect();
    Ok((candidates, schema_version))
}

#[derive(Debug, Default)]
pub(crate) struct CatalogRead {
    /// Non-empty datasheets by LCSC ID.
    pub(crate) urls: HashMap<String, String>,
    /// IDs whose `Datasheet` can't be read as text: a blob, a number, or text
    /// that isn't valid UTF-8. A catalog from `download_jlcpcb_database` has
    /// none, but a hand-made or older one can; these IDs are looked up through
    /// the LCSC API rather than failing the whole catalog. A NULL `Datasheet` is no datasheet, like an empty one.
    pub(crate) unreadable_ids: Vec<String>,
}

pub(crate) fn query_catalog_datasheets(
    db_path: &Path,
    lcsc_ids: &[String],
) -> anyhow::Result<CatalogRead> {
    let conn =
        rusqlite::Connection::open_with_flags(db_path, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)?;
    let mut stmt = conn.prepare("SELECT Datasheet FROM components WHERE LCSC = ?1 LIMIT 1")?;
    let mut read = CatalogRead::default();
    for id in lcsc_ids {
        let mut rows = stmt.query_map(rusqlite::params![id], |row| {
            Ok(match row.get_ref(0)? {
                rusqlite::types::ValueRef::Null => Some(String::new()),
                rusqlite::types::ValueRef::Text(text) => {
                    std::str::from_utf8(text).ok().map(str::to_owned)
                }
                _ => None,
            })
        })?;
        match rows.next().transpose()? {
            Some(Some(url)) => {
                let trimmed = url.trim();
                if !trimmed.is_empty() && trimmed != "~" {
                    read.urls.insert(id.clone(), trimmed.to_owned());
                }
            }
            Some(None) => read.unreadable_ids.push(id.clone()),
            None => {}
        }
    }
    Ok(read)
}

/// The parts found, how many parts with unknown stock `in_stock` left out
/// (counted only where the catalog can say so), and the catalog's version.
pub(crate) type SearchOutcome = (Vec<serde_json::Value>, Option<usize>, i64);

/// `search_jlcpcb_parts`' query: every word in `tokens` must match one of the
/// searchable columns; with no match, a punctuation-normalised retry of
/// `query`.
pub(crate) fn search_parts(
    db_path: &Path,
    query: &str,
    tokens: &[String],
    category: Option<&str>,
    basic_only: bool,
    in_stock: bool,
    limit: usize,
) -> anyhow::Result<SearchOutcome> {
    let conn =
        rusqlite::Connection::open_with_flags(db_path, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)?;
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
    for token in tokens {
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
    if let Some(cat) = category {
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

    let value_tokens: Vec<&String> = tokens
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
    let fallback_tokens: Vec<String> = query
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

        let clean: String = token
            .chars()
            .filter(|c| c.is_ascii_alphanumeric())
            .collect();
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
    if let Some(cat) = category {
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
    let fallback_rows = collect_catalog_parts(
        &mut fallback_stmt,
        &fallback_params,
        &fallback_value_tokens,
        limit,
    )?;
    // Count the unknown-stock parts of the query whose results are
    // returned: the fallback's when it found some, else the main query's.
    let fallback_unknown_stock = if fallback_rows.is_empty() {
        main_unknown_stock
    } else {
        unknown_stock(
            &fallback_unknown_stock_sql,
            &fallback_params,
            &fallback_value_tokens,
        )?
    };

    Ok((fallback_rows, fallback_unknown_stock, schema_version))
}

/// Every category with its part count and in-stock count, by name.
pub(crate) fn list_categories(db_path: &Path) -> anyhow::Result<Vec<(String, i64, i64)>> {
    let conn =
        rusqlite::Connection::open_with_flags(db_path, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)?;
    let mut stmt = conn.prepare(
        "SELECT Category, COUNT(*), SUM(Stock > 0) FROM components \
         WHERE COALESCE(Category, '') <> '' GROUP BY Category ORDER BY Category",
    )?;
    let rows = stmt
        .query_map([], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)))?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(rows)
}

/// The part with this LCSC number as the catalog tools report it, with a
/// note for a catalog from before #63, or `None` when the catalog lacks it.
pub(crate) fn get_part(db_path: &Path, lcsc_id: &str) -> anyhow::Result<Option<serde_json::Value>> {
    let conn =
        rusqlite::Connection::open_with_flags(db_path, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)?;
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
}

/// Parts, categories, schema version, and how many prices and stock counts
/// are unknown, where the catalog can say.
pub(crate) type CatalogStats = (i64, i64, i64, Option<(i64, i64)>);

/// `get_jlcpcb_database_stats`' counts.
pub(crate) fn catalog_stats(db_path: &Path) -> anyhow::Result<CatalogStats> {
    let conn =
        rusqlite::Connection::open_with_flags(db_path, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)?;
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
            |r| {
                Ok((
                    r.get::<_, Option<i64>>(0)?.unwrap_or(0),
                    r.get::<_, Option<i64>>(1)?.unwrap_or(0),
                ))
            },
        )?)
    } else {
        None
    };
    Ok((count, category_count, schema_version, unknown))
}

#[cfg(test)]
mod jlcpcb_database_tests {
    use super::*;
    use std::io::Write;

    fn seed_upstream_database(path: &Path) {
        let conn = rusqlite::Connection::open(path).expect("open upstream database");
        conn.execute_batch(
            "CREATE VIRTUAL TABLE parts USING fts5 (
                 \"LCSC Part\",
                 \"First Category\",
                 \"Second Category\",
                 \"MFR.Part\",
                 \"Package\",
                 \"Manufacturer\",
                 \"Library Type\",
                 \"Description\",
                 \"Datasheet\",
                 \"Price\",
                 \"Stock\"
             );
             INSERT INTO parts VALUES (
                 'C14663', 'Resistors', 'Chip Resistor - Surface Mount',
                 'RC0402FR-0710KL', '0402', 'YAGEO', 'Basic',
                 '10k resistor 0402', 'https://example.com/datasheet.pdf',
                 '1-9:0.012,10-99:0.008', '5000'
             );",
        )
        .expect("seed upstream database");
    }

    #[test]
    fn parses_bounded_chunk_count() {
        assert_eq!(parse_jlcpcb_chunk_count("3\n").unwrap(), 3);
        assert!(parse_jlcpcb_chunk_count("0").is_err());
        assert!(parse_jlcpcb_chunk_count("65").is_err());
        assert!(parse_jlcpcb_chunk_count("not-a-number").is_err());
    }

    #[test]
    fn extracts_single_database_from_feed_archive() {
        let dir = tempfile::tempdir().expect("tempdir");
        let archive_path = dir.path().join("parts.db.zip");
        let output_path = dir.path().join("upstream.db");
        let archive_file = std::fs::File::create(&archive_path).unwrap();
        let mut archive = zip::ZipWriter::new(archive_file);
        archive
            .start_file(
                "current-parts-fts5.db",
                zip::write::SimpleFileOptions::default()
                    .compression_method(zip::CompressionMethod::Deflated),
            )
            .unwrap();
        archive.write_all(b"SQLite database bytes").unwrap();
        archive.finish().unwrap();

        extract_jlcpcb_database(&archive_path, &output_path).unwrap();

        assert_eq!(
            std::fs::read(output_path).unwrap(),
            b"SQLite database bytes"
        );
    }

    #[test]
    fn builds_stable_components_database_from_current_feed_schema() {
        let dir = tempfile::tempdir().expect("tempdir");
        let upstream_path = dir.path().join("upstream.db");
        let output_path = dir.path().join("konnect.db");
        seed_upstream_database(&upstream_path);

        let part_count = build_konnect_jlcpcb_database(&upstream_path, &output_path).unwrap();
        assert_eq!(part_count, 1);

        let conn = rusqlite::Connection::open(output_path).expect("open generated database");
        let row: (String, f64, i64, String) = conn
            .query_row(
                "SELECT LCSC, Price, Stock, Category FROM components",
                [],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
            )
            .unwrap();
        assert_eq!(row.0, "C14663");
        assert!((row.1 - 0.012).abs() < f64::EPSILON);
        assert_eq!(row.2, 5000);
        assert_eq!(row.3, "Resistors / Chip Resistor - Surface Mount");
    }

    #[test]
    fn rejects_an_upstream_schema_missing_required_columns() {
        let dir = tempfile::tempdir().expect("tempdir");
        let upstream_path = dir.path().join("upstream.db");
        let output_path = dir.path().join("konnect.db");
        let conn = rusqlite::Connection::open(&upstream_path).expect("open upstream database");
        conn.execute("CREATE TABLE parts (\"LCSC Part\" TEXT)", [])
            .unwrap();
        drop(conn);

        let error = build_konnect_jlcpcb_database(&upstream_path, &output_path)
            .unwrap_err()
            .to_string();
        assert!(error.contains("missing required columns"));
    }

    #[test]
    fn replaces_existing_database_and_removes_temporary_backup() {
        let dir = tempfile::tempdir().expect("tempdir");
        let destination = dir.path().join("jlcpcb.db");
        let staged = dir.path().join("new.db");
        let backup = dir.path().join("previous.db");
        std::fs::write(&destination, b"old").unwrap();
        std::fs::write(&staged, b"new").unwrap();

        replace_jlcpcb_database(&staged, &destination, &backup).unwrap();

        assert_eq!(std::fs::read(destination).unwrap(), b"new");
        assert!(!backup.exists());
    }
}
