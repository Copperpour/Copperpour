//! Component sourcing (#209): the LCSC catalog, kept locally as the JLCPCB
//! parts database and online as the LCSC product API, and the EasyEDA API
//! that serves the same parts' symbols and footprints.
//!
//! [`LcscCatalog`] is the one way the tools read the local catalog, so they
//! agree on where it is and on how a missing or unreadable one is reported.
//!
//! The catalog, the LCSC API and EasyEDA come with the `jlcpcb` feature
//! (#210). Without it [`LcscCatalog`] stays, and reports every catalog as
//! missing from the build, so placement and `estimate_cost` work unchanged.

#[cfg(feature = "jlcpcb")]
pub(crate) mod catalog;
#[cfg(feature = "jlcpcb")]
pub(crate) mod easyeda;
#[cfg(feature = "jlcpcb")]
pub(crate) mod lcsc_api;
mod parts;

pub(crate) use parts::{CatalogPrices, JlcpchPartInfo};

use crate::tools::ToolContext;
use serde_json::json;
use std::path::{Path, PathBuf};

/// Why no catalog is read in a build without the `jlcpcb` feature.
#[cfg(not(feature = "jlcpcb"))]
pub(crate) const NOT_IN_THIS_BUILD: &str =
    "this build of Copperpour has no JLCPCB parts catalog (built without the 'jlcpcb' feature)";

/// The local catalog a tool call reads: the `db_path` argument, else the
/// `jlcpcb_db_path` setting, else the default path.
#[derive(Debug, Clone)]
pub(crate) struct LcscCatalog {
    path: Option<PathBuf>,
    /// The caller named the path with `db_path`.
    #[cfg(feature = "jlcpcb")]
    explicit: bool,
}

/// What reading the local catalog gave.
#[derive(Debug)]
// Without the catalog every lookup is `Missing` (#210).
#[cfg_attr(not(feature = "jlcpcb"), allow(dead_code))]
pub(crate) enum Lookup<T> {
    Read(T),
    /// No catalog to read: no path, or no file at it. The catalog is
    /// optional, so this is not an error.
    Missing(String),
    /// A catalog is there but can't be read.
    Unreadable(String),
}

impl LcscCatalog {
    pub(crate) fn resolve(args: &serde_json::Value, ctx: &ToolContext) -> Self {
        #[cfg(feature = "jlcpcb")]
        let path = catalog::resolve_db_path(args, ctx);
        #[cfg(not(feature = "jlcpcb"))]
        let path = {
            let _ = (args, ctx);
            None
        };
        Self {
            path,
            #[cfg(feature = "jlcpcb")]
            explicit: args["db_path"].as_str().is_some(),
        }
    }

    pub(crate) fn path(&self) -> Option<&Path> {
        self.path.as_deref()
    }

    /// Whether the caller named the catalog with `db_path`. A tool may refuse
    /// a missing or unreadable catalog the caller named, rather than carry on
    /// without it.
    #[cfg(feature = "jlcpcb")]
    pub(crate) fn is_explicit(&self) -> bool {
        self.explicit
    }

    /// The catalog's path when there is a file at it, or why there isn't.
    #[cfg(feature = "jlcpcb")]
    pub(crate) fn available_path(&self) -> Result<PathBuf, String> {
        self.available().map(Path::to_path_buf)
    }

    #[cfg(feature = "jlcpcb")]
    fn available(&self) -> Result<&Path, String> {
        match &self.path {
            None => Err(
                "no catalog path: jlcpcb_db_path is not set and the home directory is unknown"
                    .to_owned(),
            ),
            Some(path) if !path.is_file() => Err(format!(
                "no catalog at {}; run download_jlcpcb_database to build it",
                path.display()
            )),
            Some(path) => Ok(path),
        }
    }

    #[cfg(feature = "jlcpcb")]
    fn unreadable(&self, error: anyhow::Error) -> String {
        let path = self.path.as_deref().unwrap_or(Path::new(""));
        format!("cannot read the catalog at {}: {error:#}", path.display())
    }

    /// Runs `query` against the catalog, here.
    #[cfg(feature = "jlcpcb")]
    pub(crate) fn read_blocking<T>(
        &self,
        query: impl FnOnce(&Path) -> anyhow::Result<T>,
    ) -> Lookup<T> {
        match self.available() {
            Err(reason) => Lookup::Missing(reason),
            Ok(path) => match query(path) {
                Ok(value) => Lookup::Read(value),
                Err(error) => Lookup::Unreadable(self.unreadable(error)),
            },
        }
    }

    /// Runs `query` against the catalog, on a blocking thread.
    #[cfg(feature = "jlcpcb")]
    pub(crate) async fn read<T: Send + 'static>(
        &self,
        query: impl FnOnce(&Path) -> anyhow::Result<T> + Send + 'static,
    ) -> Lookup<T> {
        let path = match self.available() {
            Err(reason) => return Lookup::Missing(reason),
            Ok(path) => path.to_path_buf(),
        };
        let queried = tokio::task::spawn_blocking(move || query(&path))
            .await
            .map_err(anyhow::Error::from)
            .and_then(|result| result);
        match queried {
            Ok(value) => Lookup::Read(value),
            Err(error) => Lookup::Unreadable(self.unreadable(error)),
        }
    }

    /// The part with this LCSC number, for filling a placed symbol's fields.
    pub(crate) fn part_info(&self, lcsc_id: &str) -> Lookup<Option<JlcpchPartInfo>> {
        #[cfg(feature = "jlcpcb")]
        return self.read_blocking(|path| catalog::query_part_info(path, lcsc_id));
        #[cfg(not(feature = "jlcpcb"))]
        {
            let _ = lcsc_id;
            Lookup::Missing(NOT_IN_THIS_BUILD.to_owned())
        }
    }

    /// One part's datasheet, by LCSC number or else MPN.
    #[cfg(feature = "jlcpcb")]
    pub(crate) async fn datasheet(
        &self,
        lcsc_id: Option<&str>,
        mpn: Option<&str>,
    ) -> Lookup<Option<String>> {
        let (lcsc_id, mpn) = (lcsc_id.map(String::from), mpn.map(String::from));
        self.read(move |path| catalog::query_datasheet(path, lcsc_id.as_deref(), mpn.as_deref()))
            .await
    }

    /// Datasheets of several parts, by LCSC number.
    #[cfg(feature = "jlcpcb")]
    pub(crate) async fn datasheets(&self, lcsc_ids: &[String]) -> Lookup<catalog::CatalogRead> {
        let ids = lcsc_ids.to_vec();
        self.read(move |path| catalog::query_catalog_datasheets(path, &ids))
            .await
    }

    /// Prices, price breaks and library types of several parts.
    pub(crate) async fn prices(&self, lcsc_ids: &[String]) -> Lookup<CatalogPrices> {
        #[cfg(feature = "jlcpcb")]
        {
            let ids = lcsc_ids.to_vec();
            self.read(move |path| catalog::query_catalog_prices(path, &ids))
                .await
        }
        #[cfg(not(feature = "jlcpcb"))]
        {
            let _ = lcsc_ids;
            Lookup::Missing(NOT_IN_THIS_BUILD.to_owned())
        }
    }
}

impl<T> Lookup<T> {
    /// Why the catalog wasn't read, or `None` when it was.
    pub(crate) fn reason(&self) -> Option<&str> {
        match self {
            Self::Read(_) => None,
            Self::Missing(reason) | Self::Unreadable(reason) => Some(reason),
        }
    }

    /// The report the tools give of it: `{ read, path, reason }`, with `path`
    /// when there is one and `reason` when the catalog wasn't read.
    pub(crate) fn report(&self, catalog: &LcscCatalog) -> serde_json::Value {
        let mut report = json!({ "read": matches!(self, Self::Read(_)) });
        if let Some(path) = catalog.path() {
            report["path"] = json!(path);
        }
        if let Some(reason) = self.reason() {
            report["reason"] = json!(reason);
        }
        report
    }
}

#[cfg(all(test, not(feature = "jlcpcb")))]
mod not_in_this_build_tests {
    use super::*;

    /// Without the catalog, placement and pricing still get a report, saying
    /// why no catalog was read, and no path: none is looked for.
    #[tokio::test]
    async fn every_catalog_is_missing_from_the_build() {
        let ctx = ToolContext::new(
            crate::tools::ServerConfig::default(),
            std::sync::Arc::new(crate::router::ToolRouter::new()),
        );
        let catalog = LcscCatalog::resolve(&json!({}), &ctx);
        let lookup = catalog.part_info("C1");
        assert!(matches!(&lookup, Lookup::Missing(reason) if reason == NOT_IN_THIS_BUILD));
        assert_eq!(
            lookup.report(&catalog),
            json!({ "read": false, "reason": NOT_IN_THIS_BUILD })
        );
        let prices = catalog.prices(&["C1".to_owned()]).await;
        assert_eq!(prices.reason(), Some(NOT_IN_THIS_BUILD));
    }
}

#[cfg(all(test, feature = "jlcpcb"))]
mod tests {
    use super::*;

    fn catalog_at(path: Option<PathBuf>) -> LcscCatalog {
        LcscCatalog {
            path,
            explicit: false,
        }
    }

    fn seeded(dir: &Path) -> PathBuf {
        let path = dir.join("jlcpcb.db");
        let conn = rusqlite::Connection::open(&path).unwrap();
        conn.execute_batch(
            "CREATE TABLE components (LCSC TEXT, MFR_Part TEXT, Package TEXT, Manufacturer TEXT, \
             Library_Type TEXT, Description TEXT, Datasheet TEXT, Price REAL, Stock INTEGER, Category TEXT);
             INSERT INTO components VALUES ('C1', 'M1', '0402', 'Acme', 'Basic', 'd', 'https://e.com/c1.pdf', 0.1, 5, 'R');
             INSERT INTO components VALUES ('C2', 'M2', '0402', 'Acme', 'Basic', 'd', NULL, 0.1, 5, 'R');",
        )
        .unwrap();
        path
    }

    /// Every catalog lookup reports the same three outcomes, in the same words.
    #[tokio::test]
    async fn every_lookup_reports_a_missing_or_unreadable_catalog_alike() {
        let dir = tempfile::tempdir().unwrap();

        let none = catalog_at(None);
        let missing = catalog_at(Some(dir.path().join("absent.db")));
        let garbage = dir.path().join("garbage.db");
        std::fs::write(&garbage, "not a database").unwrap();
        let unreadable = catalog_at(Some(garbage.clone()));

        for (catalog, expected) in [
            (&none, "no catalog path: jlcpcb_db_path is not set"),
            (&missing, "no catalog at "),
        ] {
            let reasons = [
                catalog.part_info("C1").reason().map(String::from),
                catalog
                    .datasheet(Some("C1"), None)
                    .await
                    .reason()
                    .map(String::from),
                catalog
                    .datasheets(&["C1".into()])
                    .await
                    .reason()
                    .map(String::from),
                catalog
                    .prices(&["C1".into()])
                    .await
                    .reason()
                    .map(String::from),
            ];
            for reason in reasons {
                let reason = reason.expect("not read");
                assert!(reason.starts_with(expected), "{reason}");
            }
            assert!(matches!(catalog.part_info("C1"), Lookup::Missing(_)));
        }
        assert!(missing
            .part_info("C1")
            .reason()
            .unwrap()
            .contains("download_jlcpcb_database"));

        let lookups = [
            unreadable.part_info("C1").reason().map(String::from),
            unreadable
                .datasheet(Some("C1"), None)
                .await
                .reason()
                .map(String::from),
            unreadable
                .datasheets(&["C1".into()])
                .await
                .reason()
                .map(String::from),
            unreadable
                .prices(&["C1".into()])
                .await
                .reason()
                .map(String::from),
        ];
        for reason in lookups {
            let reason = reason.expect("not read");
            assert!(
                reason.starts_with(&format!("cannot read the catalog at {}", garbage.display())),
                "{reason}"
            );
        }
        assert!(matches!(unreadable.part_info("C1"), Lookup::Unreadable(_)));

        let report = unreadable.part_info("C1").report(&unreadable);
        assert_eq!(report["read"], false);
        assert_eq!(report["path"], json!(garbage));
        assert!(report["reason"].is_string());
        assert!(none.part_info("C1").report(&none).get("path").is_none());
    }

    #[tokio::test]
    async fn a_readable_catalog_answers_and_a_null_datasheet_is_none() {
        let dir = tempfile::tempdir().unwrap();
        let catalog = catalog_at(Some(seeded(dir.path())));

        let Lookup::Read(Some(part)) = catalog.part_info("C1") else {
            panic!("C1 not read")
        };
        assert_eq!(part.mfr_part, "M1");
        assert!(matches!(catalog.part_info("C9"), Lookup::Read(None)));
        let report = catalog.part_info("C1").report(&catalog);
        assert_eq!(report["read"], true);
        assert!(report.get("reason").is_none());

        assert!(matches!(
            catalog.datasheet(Some("C1"), None).await,
            Lookup::Read(Some(url)) if url == "https://e.com/c1.pdf"
        ));
        assert!(matches!(
            catalog.datasheet(None, Some("M1")).await,
            Lookup::Read(Some(_))
        ));
        // A NULL datasheet is no datasheet, not an unreadable catalog.
        assert!(matches!(
            catalog.datasheet(Some("C2"), None).await,
            Lookup::Read(None)
        ));
    }
}
