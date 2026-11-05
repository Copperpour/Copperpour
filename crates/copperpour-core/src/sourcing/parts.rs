//! The parts data the catalog gives, in every build: placement and
//! `estimate_cost` take it whether or not the catalog is compiled in (#210).

use std::collections::HashMap;

#[derive(Debug, Clone, Default)]
pub(crate) struct JlcpchPartInfo {
    #[allow(dead_code)]
    pub lcsc: String,
    pub mfr_part: String,
    pub manufacturer: String,
    pub datasheet: String,
    pub description: String,
}

/// A part's catalog price and library type, as `estimate_cost` prices it.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct CatalogPrice {
    /// The catalog's unit price at the first price break, `None` when it is
    /// unknown. A current catalog stores an unknown price as NULL, so its 0 is
    /// a real price the feed rounds to $0.000. One from before #63 stored an
    /// unknown price as 0 too, so there 0 is unknown.
    pub price: Option<f64>,
    pub library_type: String,
    /// The feed's price breaks as given, such as `1-9:0.012,10-:0.008`.
    /// `None` when the catalog has none for the part.
    pub price_breaks: Option<String>,
}

/// What `catalog::query_catalog_prices` read from the local catalog.
#[derive(Debug, Default)]
pub(crate) struct CatalogPrices {
    /// By LCSC ID. An ID the catalog lacks is absent.
    pub parts: HashMap<String, CatalogPrice>,
    /// Whether the catalog keeps price breaks. One from before #63 does not.
    pub has_price_breaks: bool,
}
