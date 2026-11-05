//! `estimate_cost`'s JLCPCB part prices: each part with an LCSC field from the
//! local catalog, at the price break for the order's quantity (#92).

use crate::fab::BomFootprint;
use crate::tools::ToolContext;
use serde_json::json;

/// The unit price for `quantity` from the feed's price breaks, such as
/// `1-9:0.014,10-99:0.012,100-:0.010`, and the break that gave it. Each break
/// is `min-max:price`, both ends included; an open `min-` has no upper end.
/// Below the first break, the first break's price applies, as
/// kicad-jlcpcb-tools, which builds the feed, reads it. A part sold in steps
/// has gaps between its breaks (`10-90:0.018,100-590:0.014` sells in tens),
/// and an order that falls in one rounds up to the next break, so that
/// break's price applies. `None` when the text has no break this can read,
/// or the quantity is past a closed last break.
///
/// The feed rounds prices to 3 decimals and drops every break below $0.01
/// except the first, so a cheap part has one price, and a large order of a
/// part whose price falls below $0.01 is priced at the last break kept. A
/// price under $0.0005 rounds to `0.000`, which is read as $0.
fn price_at_quantity(price_breaks: &str, quantity: u64) -> Option<BreakPrice> {
    let mut breaks: Vec<(u64, Option<u64>, f64, &str)> = Vec::new();
    for token in price_breaks.split(',').map(str::trim) {
        let (range, price) = token.split_once(':')?;
        let (min, max) = range.split_once('-')?;
        let min: u64 = min.trim().parse().ok()?;
        let max: Option<u64> = match max.trim() {
            "" => None,
            max => Some(max.parse().ok()?),
        };
        let price: f64 = price.trim().parse().ok()?;
        if !(price.is_finite() && price >= 0.0) || max.is_some_and(|max| max < min) {
            return None;
        }
        breaks.push((min, max, price, token));
    }
    breaks.sort_by_key(|&(min, ..)| min);
    let minimum_order = breaks.first()?.0;
    breaks
        .iter()
        .find(|(min, max, ..)| *min <= quantity && max.is_none_or(|max| quantity <= max))
        .or_else(|| breaks.iter().find(|(min, ..)| *min > quantity))
        .map(|&(_, _, price, token)| BreakPrice {
            price,
            price_break: token.to_string(),
            minimum_order,
        })
}

/// A unit price from [`price_at_quantity`].
#[derive(Debug, PartialEq)]
struct BreakPrice {
    price: f64,
    /// The break that gave it, as the feed writes it.
    price_break: String,
    /// The first break's lower end, the least JLCPCB sells.
    minimum_order: u64,
}

/// What the JLCPCB components cost per board, and how each part was priced.
#[derive(Debug, Default)]
pub(crate) struct JlcpcbComponentPricing {
    pub(crate) per_board: f64,
    pub(crate) extended_unique_parts: usize,
    pub(crate) priced_from_catalog: Vec<serde_json::Value>,
    pub(crate) estimated: Vec<serde_json::Value>,
    pub(crate) catalog: serde_json::Value,
}

/// Price each part with an LCSC field from the local catalog, at the price
/// break for the order's quantity of it, `boards` times its count per board;
/// every other part, and one the catalog has no price for, at `average`.
/// Without a catalog every part is at `average` (#92).
pub(crate) async fn price_jlcpcb_components(
    args: &serde_json::Value,
    ctx: &ToolContext,
    footprints: &[BomFootprint],
    average: f64,
    boards: u64,
) -> JlcpcbComponentPricing {
    let mut by_lcsc: std::collections::BTreeMap<String, Vec<String>> = Default::default();
    let mut pricing = JlcpcbComponentPricing::default();
    for footprint in footprints {
        match &footprint.lcsc {
            Some(lcsc) => by_lcsc
                .entry(lcsc.clone())
                .or_default()
                .push(footprint.reference.clone()),
            None => {
                pricing.per_board += average;
                pricing.estimated.push(json!({
                    "references": [footprint.reference],
                    "lcsc": null,
                    "reason": "no LCSC field"
                }));
            }
        }
    }

    let ids: Vec<String> = by_lcsc.keys().cloned().collect();
    let prices = if ids.is_empty() {
        pricing.catalog = json!({ "read": false, "reason": "no part has an LCSC field" });
        None
    } else {
        match crate::sourcing::catalog::resolve_db_path(args, ctx) {
            Some(db_path) if db_path.is_file() => {
                let path = db_path.clone();
                let queried = tokio::task::spawn_blocking(move || {
                    crate::sourcing::catalog::query_catalog_prices(&path, &ids)
                })
                .await
                .map_err(anyhow::Error::from)
                .and_then(|result| result);
                match queried {
                    Ok(prices) => {
                        pricing.catalog = if prices.has_price_breaks {
                            json!({ "read": true, "path": db_path, "price_breaks": true })
                        } else {
                            json!({
                                "read": true,
                                "path": db_path,
                                "price_breaks": false,
                                "reason": "the catalog was built before price breaks were kept, so parts are at their first break's price; re-run download_jlcpcb_database with force to rebuild it"
                            })
                        };
                        Some(prices.parts)
                    }
                    Err(error) => {
                        pricing.catalog = json!({
                            "read": false,
                            "path": db_path,
                            "reason": format!("cannot read the catalog: {error:#}")
                        });
                        None
                    }
                }
            }
            Some(db_path) => {
                pricing.catalog = json!({
                    "read": false,
                    "path": db_path,
                    "reason": "no catalog at this path; run download_jlcpcb_database"
                });
                None
            }
            None => {
                pricing.catalog = json!({
                    "read": false,
                    "reason": "no catalog path: jlcpcb_db_path is not set and the home directory is unknown"
                });
                None
            }
        }
    };

    for (lcsc, references) in by_lcsc {
        let count = references.len() as f64;
        let found = prices.as_ref().and_then(|prices| prices.get(&lcsc));
        // The fee follows the library type, whether or not the price is known.
        if found.is_some_and(|part| part.library_type == "Extended") {
            pricing.extended_unique_parts += 1;
        }
        match found.and_then(|part| Some((part, part.price?))) {
            Some((part, first_price)) => {
                let order_quantity = (references.len() as u64).saturating_mul(boards);
                // A part whose breaks can't be read keeps the first break's
                // price, which the import stores on its own.
                let at_quantity = part
                    .price_breaks
                    .as_deref()
                    .and_then(|breaks| price_at_quantity(breaks, order_quantity));
                let unit_price = at_quantity.as_ref().map_or(first_price, |at| at.price);
                pricing.per_board += unit_price * count;
                let mut entry = json!({
                    "lcsc": lcsc,
                    "references": references,
                    "library_type": part.library_type,
                    "order_quantity": order_quantity,
                    "unit_price": unit_price,
                    "price_break": at_quantity.as_ref().map(|at| &at.price_break)
                });
                // JLCPCB sells at least the first break's quantity; the
                // estimate prices only the parts the boards use.
                if let Some(at) = at_quantity
                    .as_ref()
                    .filter(|at| order_quantity < at.minimum_order)
                {
                    entry["below_minimum_order"] = json!(at.minimum_order);
                }
                if unit_price == 0.0 {
                    entry["note"] = json!(
                        "the feed rounds this part's price to $0.000, so it is under $0.0005"
                    );
                }
                pricing.priced_from_catalog.push(entry);
            }
            None => {
                pricing.per_board += average * count;
                let reason = match (&prices, found) {
                    (None, _) => "catalog not read",
                    (Some(_), None) => "not in the catalog",
                    (Some(_), Some(_)) => "the catalog has no price",
                };
                pricing.estimated.push(json!({
                    "references": references,
                    "lcsc": lcsc,
                    "reason": reason
                }));
            }
        }
    }
    pricing
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn price_breaks_resolve_by_quantity() {
        let breaks = "1-9:0.12,10-99:0.08,100-:0.05";
        for (quantity, price, token) in [
            (1, 0.12, "1-9:0.12"),
            (9, 0.12, "1-9:0.12"),
            (10, 0.08, "10-99:0.08"),
            (99, 0.08, "10-99:0.08"),
            (100, 0.05, "100-:0.05"),
            (1_000_000, 0.05, "100-:0.05"),
        ] {
            assert_eq!(
                price_at_quantity(breaks, quantity).map(|at| (at.price, at.price_break)),
                Some((price, token.to_string())),
                "{quantity}"
            );
        }
        // Below the first break, and out of order, the first break applies.
        assert_eq!(
            price_at_quantity("100-:0.05,20-99:0.07", 5).map(|at| (at.price, at.price_break)),
            Some((0.07, "20-99:0.07".to_string()))
        );
        // C2141 in the 2026-11-04 feed sells in tens: 95 rounds up to 100.
        let tens = "10-90:0.018,100-590:0.014,600-1190:0.012,1200-:0.010";
        assert_eq!(
            price_at_quantity(tens, 95).map(|at| (at.price, at.price_break)),
            Some((0.014, "100-590:0.014".to_string()))
        );
        assert_eq!(
            price_at_quantity(tens, 90).map(|at| (at.price, at.price_break)),
            Some((0.018, "10-90:0.018".to_string()))
        );
        // A price the feed rounds to zero is $0; the first break's lower end
        // is the minimum order.
        assert_eq!(
            price_at_quantity("1-:0.000", 5),
            Some(BreakPrice {
                price: 0.0,
                price_break: "1-:0.000".to_string(),
                minimum_order: 1
            })
        );
        assert_eq!(
            price_at_quantity("10-49:0.180,50-:0.144", 5).map(|at| at.minimum_order),
            Some(10)
        );
        // A single closed break: a quantity above it has no price.
        assert_eq!(price_at_quantity("1-9:0.12", 10), None);
        for unreadable in [
            "",
            "0.12",
            "1-9",
            "a-9:0.12",
            "1-9:x",
            "9-1:0.12",
            "1-:-0.1",
            "1-:NaN",
            "1-9:0.1,10:0.2",
        ] {
            assert_eq!(price_at_quantity(unreadable, 5), None, "{unreadable}");
        }
    }
}
