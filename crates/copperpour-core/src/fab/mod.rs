//! Fabricators as data (#209).
//!
//! Most of what differs between fab houses is numbers: design-rule limits and
//! a rough price model. Each fab house is one [`FabProfile`], so adding one
//! that differs only in those numbers is one table entry.
//!
//! Only JLCPCB has assembly output of its own, in [`jlcpcb`]. Every other fab
//! house gets KiCad's native BOM and position files. [`AssemblyFormat`] holds
//! that choice, so `export_manufacturing_package` doesn't branch on the fab
//! house itself.

pub(crate) mod jlcpcb;

use crate::tools::{cli, ToolContext};
use std::collections::BTreeSet;
use std::path::Path;

/// A footprint a BOM lists, by reference and LCSC field.
pub(crate) struct BomFootprint {
    pub(crate) reference: String,
    pub(crate) lcsc: Option<String>,
}

/// What `validate_for_manufacturing` and `estimate_cost` know about a fab house.
#[derive(Debug)]
pub(crate) struct FabProfile {
    /// The `fab_house` value that selects this profile.
    pub id: &'static str,
    /// The narrowest trace the fab house builds, in mm.
    pub min_trace_mm: f64,
    /// How `estimate_cost` prices an order.
    pub cost_model: &'static CostModel,
    /// What `export_manufacturing_package` writes for assembly.
    pub assembly: AssemblyFormat,
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

#[cfg(feature = "jlcpcb")]
const JLCPCB_EXTENDED_FEE_SOURCE: &str = "JLCPCB Economic PCBA: $3.07 per unique Extended part, none for Basic or Preferred Extended parts (https://jlcpcb.com/blog/reduce-pcb-assembly-cost-prototype, prices as of 2026-10-09)";
/// Without the catalog no part's library type is known, so no Extended fee is
/// counted, and the offline build carries no vendor URL (#210).
#[cfg(not(feature = "jlcpcb"))]
const JLCPCB_EXTENDED_FEE_SOURCE: &str = "JLCPCB Economic PCBA charges $3.07 per unique Extended part, none for Basic or Preferred Extended parts (prices as of 2026-10-09). This build has no JLCPCB catalog, so it doesn't know which parts are Extended and counts no such fee";

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
        fee_source: JLCPCB_EXTENDED_FEE_SOURCE,
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
        assembly: AssemblyFormat::Jlcpcb,
    },
    FabProfile {
        id: "oshpark",
        min_trace_mm: 0.152,
        cost_model: &GENERIC_COSTS,
        assembly: AssemblyFormat::KicadNative,
    },
    FabProfile {
        id: "pcbway",
        min_trace_mm: 0.1,
        cost_model: &PCBWAY_COSTS,
        assembly: AssemblyFormat::KicadNative,
    },
];

/// For any `fab_house` without a profile of its own, `generic` included.
const GENERIC: FabProfile = FabProfile {
    id: "generic",
    min_trace_mm: 0.15,
    cost_model: &GENERIC_COSTS,
    assembly: AssemblyFormat::KicadNative,
};

/// The profile for a `fab_house` value, or the generic one.
pub(crate) fn profile(fab_house: &str) -> &'static FabProfile {
    PROFILES
        .iter()
        .find(|profile| profile.id == fab_house)
        .unwrap_or(&GENERIC)
}

/// The assembly files `export_manufacturing_package` writes for a fab house.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum AssemblyFormat {
    /// KiCad's own position CSV and BOM, as kicad-cli writes them.
    KicadNative,
    /// JLCPCB's CPL from native pad midpoints with rotation corrections, and a
    /// BOM that lists every reference ([`jlcpcb`]).
    Jlcpcb,
}

impl AssemblyFormat {
    /// The refusal for arguments this format can't honour, as
    /// `(field, reason)`, before anything is written.
    pub(crate) fn check_arguments(
        self,
        include_assembly: bool,
        position_units: &str,
        has_cpl_corrections: bool,
    ) -> Result<(), (&'static str, &'static str)> {
        let jlcpcb = self == Self::Jlcpcb;
        if jlcpcb && include_assembly && position_units != "mm" {
            return Err((
                "position_units",
                "JLCPCB CPL coordinates must use millimetres",
            ));
        }
        if has_cpl_corrections && (!jlcpcb || !include_assembly) {
            return Err((
                "jlcpcb_cpl_corrections_path",
                "requires fab_house='jlcpcb' and include_assembly=true",
            ));
        }
        Ok(())
    }

    /// The live board snapshot this format exports from, captured before any
    /// output is written: JLCPCB assembly needs native pad midpoints over IPC.
    /// The inner `Err` is why the board couldn't be read.
    pub(crate) async fn capture(
        self,
        ctx: &ToolContext,
        board: &Path,
        include_assembly: bool,
    ) -> anyhow::Result<Result<Option<jlcpcb::midpoints::Snapshot>, copperpour_ipc::IpcFailure>>
    {
        if self != Self::Jlcpcb || !include_assembly {
            return Ok(Ok(None));
        }
        let captured =
            crate::tools::with_board_ipc_classified(ctx, board, jlcpcb::midpoints::capture).await?;
        Ok(captured.map(Some))
    }

    /// The position file's name.
    pub(crate) fn positions_file_name(self, project: &str) -> String {
        match self {
            Self::KicadNative => "positions.csv".to_string(),
            Self::Jlcpcb => format!("CPL-{project}.csv"),
        }
    }

    /// The BOM's name.
    pub(crate) fn bom_file_name(self, project: &str) -> String {
        match self {
            Self::KicadNative => "bom.csv".to_string(),
            Self::Jlcpcb => format!("BOM-{project}.csv"),
        }
    }

    /// Writes the position file. JLCPCB's carries the designators it lists and
    /// the orientation evidence; KiCad's native file has neither.
    #[allow(clippy::too_many_arguments)]
    pub(crate) async fn export_positions(
        self,
        cli_path: &str,
        board: &Path,
        export_board: &Path,
        output: &Path,
        side: &str,
        units: &str,
        cpl_corrections: Option<&Path>,
        snapshot: Option<&jlcpcb::midpoints::Snapshot>,
    ) -> anyhow::Result<Option<jlcpcb::JlcpcbCplExport>> {
        match self {
            Self::KicadNative => {
                cli::export_position_file(cli_path, board, output, "csv", units, side).await?;
                Ok(None)
            }
            Self::Jlcpcb => {
                use anyhow::Context;
                jlcpcb::export_jlcpcb_cpl(
                    cli_path,
                    export_board,
                    output,
                    side,
                    cpl_corrections,
                    &snapshot.context("JLCPCB snapshot missing")?.midpoints,
                )
                .await
                .map(Some)
            }
        }
    }

    /// Whether the BOM leaves out `dnp` parts.
    pub(crate) fn bom_excludes_dnp(self) -> bool {
        self == Self::Jlcpcb
    }

    /// Writes the BOM. JLCPCB's lists every reference, without KiCad's
    /// compressed ranges.
    pub(crate) async fn export_bom(
        self,
        cli_path: &str,
        schematic: &Path,
        output: &Path,
        options: &cli::BomOptions<'_>,
    ) -> anyhow::Result<()> {
        match self {
            Self::KicadNative => cli::export_bom(cli_path, schematic, output, options).await,
            Self::Jlcpcb => {
                cli::export_bom_with_ref_range_delimiter(cli_path, schematic, output, options, "")
                    .await
            }
        }
    }

    /// The written BOM's designators, for the BOM/CPL cross-check, or `None`
    /// when this format has no check. The outer `Err` is a failure to read the
    /// BOM; the inner one is a BOM this format refuses.
    pub(crate) async fn bom_designators(
        self,
        bom: &Path,
        cpl_designators: Option<&BTreeSet<String>>,
    ) -> anyhow::Result<anyhow::Result<Option<BTreeSet<String>>>> {
        match self {
            Self::KicadNative => Ok(Ok(None)),
            Self::Jlcpcb => {
                let source = tokio::fs::read_to_string(bom).await?;
                Ok(jlcpcb::jlcpcb_bom_designators(&source, cpl_designators).map(Some))
            }
        }
    }

    /// The warning for a BOM and CPL that name different designators.
    pub(crate) fn designator_mismatch(
        self,
        cpl: &BTreeSet<String>,
        bom: &BTreeSet<String>,
    ) -> Option<String> {
        match self {
            Self::KicadNative => None,
            Self::Jlcpcb => jlcpcb::jlcpcb_designator_mismatch(cpl, bom),
        }
    }

    /// What to do after uploading an assembly package, beyond uploading it.
    pub(crate) fn after_upload(self) -> Option<&'static str> {
        match self {
            Self::KicadNative => None,
            Self::Jlcpcb => Some(
                "Then inspect every component in JLCPCB Component Placements, starting with the parts named in `warnings`. Correction rules reduce known orientation mismatches; they do not replace the mandatory visual placement preview.",
            ),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Each fab house's limits and prices. The first entries are the numbers
    /// the `match fab_house` tables held before #209, so moving them into
    /// profiles changed no limit and no price.
    #[test]
    fn profiles_hold_each_fab_houses_limits_and_prices() {
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

    /// The steps `export_manufacturing_package` takes from the format, which
    /// its own tests reach only with KiCad open.
    #[test]
    fn assembly_formats_name_their_files_and_filter_their_bom() {
        let native = profile("pcbway").assembly;
        let jlcpcb = profile("jlcpcb").assembly;
        assert_eq!(native, AssemblyFormat::KicadNative);
        assert_eq!(jlcpcb, AssemblyFormat::Jlcpcb);
        assert_eq!(native.positions_file_name("demo"), "positions.csv");
        assert_eq!(native.bom_file_name("demo"), "bom.csv");
        assert_eq!(jlcpcb.positions_file_name("demo"), "CPL-demo.csv");
        assert_eq!(jlcpcb.bom_file_name("demo"), "BOM-demo.csv");
        assert!(!native.bom_excludes_dnp());
        assert!(jlcpcb.bom_excludes_dnp());
        assert!(native.after_upload().is_none());
        assert!(jlcpcb
            .after_upload()
            .is_some_and(|then| then.contains("Component Placements")));
    }

    /// JLCPCB checks the written BOM and compares it with the CPL; KiCad's
    /// native format checks neither.
    #[tokio::test]
    async fn only_jlcpcb_checks_the_bom_against_the_cpl() {
        let dir = tempfile::tempdir().unwrap();
        let bom = dir.path().join("bom.csv");
        let jlcpcb = AssemblyFormat::Jlcpcb;
        let native = AssemblyFormat::KicadNative;

        std::fs::write(&bom, "Designator,Comment\n\"R1,R2\",10k\n").unwrap();
        let read = jlcpcb.bom_designators(&bom, None).await.unwrap().unwrap();
        assert_eq!(read, Some(BTreeSet::from(["R1".into(), "R2".into()])));
        assert_eq!(
            native.bom_designators(&bom, None).await.unwrap().unwrap(),
            None
        );

        // A compressed range is refused for JLCPCB only.
        std::fs::write(&bom, "Designator,Comment\n\"R1-R3\",10k\n").unwrap();
        assert!(jlcpcb.bom_designators(&bom, None).await.unwrap().is_err());
        assert_eq!(
            native.bom_designators(&bom, None).await.unwrap().unwrap(),
            None
        );

        let cpl = BTreeSet::from(["R1".to_string(), "R2".to_string()]);
        let bom_refs = BTreeSet::from(["R1".to_string()]);
        assert!(jlcpcb
            .designator_mismatch(&cpl, &bom_refs)
            .is_some_and(|warning| warning.contains("R2")));
        assert_eq!(jlcpcb.designator_mismatch(&cpl, &cpl), None);
        assert_eq!(native.designator_mismatch(&cpl, &bom_refs), None);
    }
}
