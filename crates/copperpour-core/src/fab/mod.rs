//! Fabricators as data (#209).
//!
//! Most of what differs between fab houses is numbers: design-rule limits and
//! a rough price model. Each fab house is one [`FabProfile`], so adding one
//! that differs only in those numbers is one table entry.

/// What `validate_for_manufacturing` and `estimate_cost` know about a fab house.
#[derive(Debug)]
pub(crate) struct FabProfile {
    /// The `fab_house` value that selects this profile.
    pub id: &'static str,
    /// The narrowest trace the fab house builds, in mm.
    pub min_trace_mm: f64,
    /// How `estimate_cost` prices an order.
    pub cost_model: &'static CostModel,
}

/// A rough price model for `estimate_cost`.
#[derive(Debug)]
pub(crate) struct CostModel {
    /// Where the PCB and assembly prices come from, with a date. `None` means
    /// they are rough figures with no cited source, and the estimate says it
    /// is approximate (#209).
    pub source: Option<&'static str>,
    pub pcb: PcbPrice,
    /// `None` when the fab house's assembly isn't priced.
    pub assembly: Option<AssemblyPrice>,
    pub components: ComponentPrice,
}

/// The price of the bare boards: a base price that includes some boards, and
/// a price for each board beyond them, by copper layer count.
#[derive(Debug)]
pub(crate) struct PcbPrice {
    /// How many boards the base price includes.
    pub included_boards: f64,
    /// `(copper layer counts, base, per extra board)`; the first entry whose
    /// counts include the board's applies.
    pub by_layers: &'static [(&'static [usize], f64, f64)],
    /// `(base, per extra board)` for any other layer count.
    pub other_layers: (f64, f64),
}

impl PcbPrice {
    pub(crate) fn cost(&self, copper_layers: usize, quantity: usize) -> f64 {
        let (base, per_board) = self
            .by_layers
            .iter()
            .find(|(layers, ..)| layers.contains(&copper_layers))
            .map_or(self.other_layers, |&(_, base, per_board)| (base, per_board));
        base + (quantity as f64 - self.included_boards).max(0.0) * per_board
    }
}

/// The price of placing parts.
#[derive(Debug)]
pub(crate) struct AssemblyPrice {
    /// Charged once per order when any part is placed.
    pub setup: f64,
    /// Per placed part, per board.
    pub per_part: f64,
}

impl AssemblyPrice {
    pub(crate) fn cost(&self, placed_parts: usize, quantity: usize) -> f64 {
        let setup = if placed_parts > 0 { self.setup } else { 0.0 };
        setup + placed_parts as f64 * self.per_part * quantity as f64
    }
}

/// How the parts themselves are priced.
#[derive(Debug)]
pub(crate) enum ComponentPrice {
    /// Not priced.
    None,
    /// The same price for every part the BOM lists.
    PerPart(f64),
    /// Each part with an LCSC field from the local JLCPCB catalog, the rest at
    /// `average`, plus a fee per unique Extended part.
    JlcpcbCatalog {
        average: f64,
        extended_part_fee: f64,
        fee_source: &'static str,
    },
}

const JLCPCB_COSTS: CostModel = CostModel {
    source: None,
    pcb: PcbPrice {
        included_boards: 5.0,
        // JLCPCB prices single-sided boards on the two-layer scale.
        by_layers: &[(&[1, 2], 2.0, 0.40), (&[4], 7.0, 1.40), (&[6], 15.0, 3.00)],
        other_layers: (30.0, 5.00),
    },
    assembly: Some(AssemblyPrice {
        setup: 8.0,
        per_part: 0.003,
    }),
    components: ComponentPrice::JlcpcbCatalog {
        average: 0.05,
        // JLCPCB's loading fee per unique Extended part under Economic PCBA,
        // whose setup fee the estimate assumes. Basic and Preferred Extended
        // parts have none.
        extended_part_fee: 3.07,
        fee_source: "JLCPCB Economic PCBA: $3.07 per unique Extended part, none for Basic or Preferred Extended parts (https://jlcpcb.com/blog/reduce-pcb-assembly-cost-prototype, prices as of 2026-10-09)",
    },
};

const PCBWAY_COSTS: CostModel = CostModel {
    source: None,
    pcb: PcbPrice {
        included_boards: 5.0,
        by_layers: &[(&[1, 2], 5.0, 0.50), (&[4], 12.0, 2.00)],
        other_layers: (25.0, 4.00),
    },
    assembly: Some(AssemblyPrice {
        setup: 0.0,
        per_part: 0.005,
    }),
    components: ComponentPrice::PerPart(0.08),
};

/// For a fab house without a price model of its own, OSH Park included.
const GENERIC_COSTS: CostModel = CostModel {
    source: None,
    pcb: PcbPrice {
        included_boards: 0.0,
        by_layers: &[],
        other_layers: (10.0, 2.0),
    },
    assembly: None,
    components: ComponentPrice::None,
};

const PROFILES: &[FabProfile] = &[
    FabProfile {
        id: "jlcpcb",
        min_trace_mm: 0.127,
        cost_model: &JLCPCB_COSTS,
    },
    FabProfile {
        id: "oshpark",
        min_trace_mm: 0.152,
        cost_model: &GENERIC_COSTS,
    },
    FabProfile {
        id: "pcbway",
        min_trace_mm: 0.1,
        cost_model: &PCBWAY_COSTS,
    },
];

/// For any `fab_house` without a profile of its own, `generic` included.
const GENERIC: FabProfile = FabProfile {
    id: "generic",
    min_trace_mm: 0.15,
    cost_model: &GENERIC_COSTS,
};

/// The profile for a `fab_house` value, or the generic one.
pub(crate) fn profile(fab_house: &str) -> &'static FabProfile {
    PROFILES
        .iter()
        .find(|profile| profile.id == fab_house)
        .unwrap_or(&GENERIC)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The numbers the `match fab_house` tables held before #209, so moving
    /// them into profiles changes no limit and no price.
    #[test]
    fn profiles_keep_the_former_tables() {
        for (fab_house, min_trace) in [
            ("jlcpcb", 0.127),
            ("oshpark", 0.152),
            ("pcbway", 0.1),
            ("generic", 0.15),
            ("unknown", 0.15),
        ] {
            assert_eq!(profile(fab_house).min_trace_mm, min_trace, "{fab_house}");
        }

        // (fab house, layers, quantity) → PCB price, as computed before.
        for (fab_house, layers, quantity, pcb) in [
            ("jlcpcb", 1, 5, 2.0),
            ("jlcpcb", 2, 10, 2.0 + 5.0 * 0.40),
            ("jlcpcb", 4, 3, 7.0),
            ("jlcpcb", 6, 7, 15.0 + 2.0 * 3.00),
            ("jlcpcb", 8, 6, 30.0 + 5.00),
            ("jlcpcb", 0, 5, 30.0),
            ("pcbway", 2, 6, 5.0 + 0.50),
            ("pcbway", 4, 5, 12.0),
            ("pcbway", 6, 9, 25.0 + 4.0 * 4.00),
            ("oshpark", 2, 3, 10.0 + 3.0 * 2.0),
            ("generic", 4, 1, 10.0 + 2.0),
            ("unknown", 2, 5, 10.0 + 5.0 * 2.0),
        ] {
            assert_eq!(
                profile(fab_house).cost_model.pcb.cost(layers, quantity),
                pcb,
                "{fab_house} {layers} layers x{quantity}"
            );
        }

        let jlcpcb = profile("jlcpcb").cost_model.assembly.as_ref().unwrap();
        assert_eq!(jlcpcb.cost(7, 5), 8.0 + 7.0 * 0.003 * 5.0);
        assert_eq!(jlcpcb.cost(0, 5), 0.0);
        let pcbway = profile("pcbway").cost_model.assembly.as_ref().unwrap();
        assert_eq!(pcbway.cost(7, 5), 7.0 * 0.005 * 5.0);
        assert!(profile("generic").cost_model.assembly.is_none());
        assert!(matches!(
            profile("pcbway").cost_model.components,
            ComponentPrice::PerPart(0.08)
        ));
        assert!(matches!(
            profile("oshpark").cost_model.components,
            ComponentPrice::None
        ));
    }
}
