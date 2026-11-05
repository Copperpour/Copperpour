//! JLCPCB assembly output (#209): the CPL from native pad midpoints, its
//! rotation corrections, and the checks on the BOM and CPL designators.
//!
//! `export_manufacturing_package` reaches this through
//! [`super::AssemblyFormat::Jlcpcb`].

pub(crate) mod midpoints;
pub(crate) mod pricing;

use crate::tools::cli;
use anyhow::{bail, Context};
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;
use std::path::Path;

pub(crate) async fn export_jlcpcb_cpl(
    cli_path: &str,
    board: &Path,
    output: &Path,
    side: &str,
    project_policy_path: Option<&Path>,
    midpoints: &std::collections::BTreeMap<String, midpoints::Midpoint>,
) -> anyhow::Result<JlcpcbCplExport> {
    let staging = tempfile::tempdir_in(
        output
            .parent()
            .context("JLCPCB CPL output has no parent directory")?,
    )?;
    let native = staging.path().join("kicad-positions.csv");
    cli::export_position_file_excluding_dnp(cli_path, board, &native, "csv", "mm", side).await?;
    let source = tokio::fs::read_to_string(&native).await?;
    let policies = JlcpcbCorrectionPolicies::load(project_policy_path).await?;
    let (source, geometry) = midpoints::convert_positions(&source, midpoints)?;
    let mut export = jlcpcb_cpl_from_kicad_csv(&source, &policies)?;
    export.orientation_evidence.geometry = geometry;
    cli::publish_verified_bytes(output, &export.bytes, "JLCPCB CPL").await?;
    Ok(export)
}

const BUILT_IN_JLCPCB_CORRECTIONS: &str = include_str!("cpl_corrections_v1.json");

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct JlcpcbCorrectionPolicy {
    schema_version: u64,
    policy_id: String,
    provenance: String,
    #[serde(default)]
    footprint_rules: Vec<JlcpcbCorrectionRule>,
    #[serde(default)]
    component_overrides: Vec<JlcpcbComponentOverride>,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct JlcpcbCorrectionRule {
    id: String,
    footprint_prefix: String,
    #[serde(default)]
    rotation_degrees: f64,
    #[serde(default)]
    offset_x_mm: f64,
    #[serde(default)]
    offset_y_mm: f64,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct JlcpcbComponentOverride {
    id: String,
    designator: String,
    #[serde(default)]
    rotation_degrees: f64,
    #[serde(default)]
    offset_x_mm: f64,
    #[serde(default)]
    offset_y_mm: f64,
}

#[derive(Debug)]
struct JlcpcbCorrectionPolicies {
    built_in: JlcpcbCorrectionPolicy,
    project: Option<JlcpcbCorrectionPolicy>,
}

impl JlcpcbCorrectionPolicies {
    async fn load(project_path: Option<&Path>) -> anyhow::Result<Self> {
        let built_in = parse_jlcpcb_correction_policy(BUILT_IN_JLCPCB_CORRECTIONS, "built-in")?;
        let project = if let Some(path) = project_path {
            let source = tokio::fs::read_to_string(path)
                .await
                .with_context(|| format!("read JLCPCB correction policy {}", path.display()))?;
            Some(parse_jlcpcb_correction_policy(
                &source,
                &path.display().to_string(),
            )?)
        } else {
            None
        };
        Ok(Self { built_in, project })
    }

    fn matching_rule<'a>(&'a self, designator: &str, footprint: &str) -> Option<MatchedRule<'a>> {
        if let Some(project) = &self.project {
            if let Some(rule) = project
                .component_overrides
                .iter()
                .find(|rule| rule.designator == designator)
            {
                return Some(MatchedRule::Component {
                    policy: project,
                    rule,
                });
            }
            if let Some(rule) = project
                .footprint_rules
                .iter()
                .find(|rule| footprint.starts_with(&rule.footprint_prefix))
            {
                return Some(MatchedRule::Footprint {
                    policy: project,
                    rule,
                });
            }
        }
        self.built_in
            .footprint_rules
            .iter()
            .find(|rule| footprint.starts_with(&rule.footprint_prefix))
            .map(|rule| MatchedRule::Footprint {
                policy: &self.built_in,
                rule,
            })
    }
}

enum MatchedRule<'a> {
    Component {
        policy: &'a JlcpcbCorrectionPolicy,
        rule: &'a JlcpcbComponentOverride,
    },
    Footprint {
        policy: &'a JlcpcbCorrectionPolicy,
        rule: &'a JlcpcbCorrectionRule,
    },
}

impl MatchedRule<'_> {
    fn values(&self) -> (&str, &str, &str, f64, f64, f64) {
        match self {
            Self::Component { policy, rule } => (
                &policy.policy_id,
                &rule.id,
                "component_override",
                rule.rotation_degrees,
                rule.offset_x_mm,
                rule.offset_y_mm,
            ),
            Self::Footprint { policy, rule } => (
                &policy.policy_id,
                &rule.id,
                "footprint_rule",
                rule.rotation_degrees,
                rule.offset_x_mm,
                rule.offset_y_mm,
            ),
        }
    }
}

fn parse_jlcpcb_correction_policy(
    source: &str,
    description: &str,
) -> anyhow::Result<JlcpcbCorrectionPolicy> {
    let policy: JlcpcbCorrectionPolicy = serde_json::from_str(source)
        .with_context(|| format!("parse JLCPCB correction policy {description}"))?;
    if policy.schema_version != 1 {
        bail!(
            "JLCPCB correction policy {description} uses unsupported schema_version {}",
            policy.schema_version
        );
    }
    if policy.policy_id.trim().is_empty() || policy.provenance.trim().is_empty() {
        bail!("JLCPCB correction policy {description} requires policy_id and provenance");
    }
    let mut ids = BTreeSet::new();
    for rule in &policy.footprint_rules {
        if rule.id.trim().is_empty() || rule.footprint_prefix.trim().is_empty() {
            bail!(
                "JLCPCB correction policy {description} has an empty rule id or footprint_prefix"
            );
        }
        if !ids.insert(rule.id.as_str()) {
            bail!(
                "JLCPCB correction policy {description} has duplicate rule id '{}'",
                rule.id
            );
        }
        validate_finite_correction(
            description,
            &rule.id,
            rule.rotation_degrees,
            rule.offset_x_mm,
            rule.offset_y_mm,
        )?;
    }
    let mut designators = BTreeSet::new();
    for rule in &policy.component_overrides {
        if rule.id.trim().is_empty() || rule.designator.trim().is_empty() {
            bail!("JLCPCB correction policy {description} has an empty override id or designator");
        }
        if !ids.insert(rule.id.as_str()) {
            bail!(
                "JLCPCB correction policy {description} has duplicate rule id '{}'",
                rule.id
            );
        }
        if !designators.insert(rule.designator.as_str()) {
            bail!(
                "JLCPCB correction policy {description} has duplicate override for '{}'",
                rule.designator
            );
        }
        validate_finite_correction(
            description,
            &rule.id,
            rule.rotation_degrees,
            rule.offset_x_mm,
            rule.offset_y_mm,
        )?;
    }
    Ok(policy)
}

fn validate_finite_correction(
    description: &str,
    id: &str,
    rotation: f64,
    offset_x: f64,
    offset_y: f64,
) -> anyhow::Result<()> {
    if [rotation, offset_x, offset_y]
        .iter()
        .any(|value| !value.is_finite())
    {
        bail!("JLCPCB correction rule '{id}' in {description} contains a non-finite value");
    }
    Ok(())
}

#[derive(Clone, Debug, Serialize)]
pub(crate) struct JlcpcbOrientationEvidence {
    status: &'static str,
    physical_validation: bool,
    geometry: Vec<midpoints::Midpoint>,
    policies: Vec<JlcpcbPolicyEvidence>,
    applied_corrections: Vec<JlcpcbAppliedCorrection>,
    unmatched_footprints: Vec<JlcpcbUnmatchedFootprint>,
    note: &'static str,
}

#[derive(Clone, Debug, Serialize)]
struct JlcpcbPolicyEvidence {
    policy_id: String,
    schema_version: u64,
    provenance: String,
    precedence: &'static str,
}

#[derive(Clone, Debug, Serialize)]
struct JlcpcbAppliedCorrection {
    designator: String,
    footprint: String,
    side: String,
    policy_id: String,
    rule_id: String,
    match_kind: String,
    rotation_before_degrees: f64,
    rotation_after_degrees: f64,
    x_before_mm: f64,
    x_after_mm: f64,
    y_before_mm: f64,
    y_after_mm: f64,
}

#[derive(Clone, Debug, Serialize)]
struct JlcpcbUnmatchedFootprint {
    designator: String,
    footprint: String,
    side: String,
}

#[derive(Debug)]
pub(crate) struct JlcpcbCplExport {
    pub(crate) bytes: Vec<u8>,
    pub(crate) designators: BTreeSet<String>,
    pub(crate) orientation_evidence: JlcpcbOrientationEvidence,
}

/// Translate KiCad 10's native position CSV into JLCPCB's documented CPL
/// contract: Designator, Mid X, Mid Y, Layer, Rotation. Parsing and writing as
/// CSV preserves quoted commas and non-ASCII values even though Val/Package do
/// not belong in the vendor file.
fn jlcpcb_cpl_from_kicad_csv(
    source: &str,
    policies: &JlcpcbCorrectionPolicies,
) -> anyhow::Result<JlcpcbCplExport> {
    let mut reader = csv::ReaderBuilder::new()
        .trim(csv::Trim::All)
        .from_reader(source.as_bytes());
    let headers = reader.headers()?.clone();
    let column = |name: &str| {
        headers
            .iter()
            .position(|header| header == name)
            .with_context(|| format!("KiCad position CSV is missing '{name}'"))
    };
    let reference = column("Ref")?;
    let pos_x = column("PosX")?;
    let pos_y = column("PosY")?;
    let rotation = column("Rot")?;
    let side = column("Side")?;
    let package = column("Package")?;

    let mut writer = csv::WriterBuilder::new().from_writer(Vec::new());
    writer.write_record(["Designator", "Mid X", "Mid Y", "Layer", "Rotation"])?;
    let mut designators = BTreeSet::new();
    let mut applied_corrections = Vec::new();
    let mut unmatched_footprints = Vec::new();
    for row in reader.records() {
        let row = row?;
        let field = |index: usize, name: &str| {
            row.get(index)
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .with_context(|| format!("KiCad position row is missing '{name}'"))
        };
        let designator = field(reference, "Ref")?;
        let footprint = field(package, "Package")?
            .rsplit(':')
            .next()
            .context("footprint basename is missing")?;
        let x_source = field(pos_x, "PosX")?;
        let y_source = field(pos_y, "PosY")?;
        let angle_source = field(rotation, "Rot")?;
        let mut x = x_source
            .parse::<f64>()
            .with_context(|| format!("invalid PosX for {designator}: {x_source}"))?;
        let mut y = y_source
            .parse::<f64>()
            .with_context(|| format!("invalid PosY for {designator}: {y_source}"))?;
        let angle = angle_source
            .parse::<f64>()
            .with_context(|| format!("invalid Rot for {designator}: {angle_source}"))?;
        let layer = match field(side, "Side")?.to_ascii_lowercase().as_str() {
            "top" | "front" => "top",
            "bottom" | "back" => "bottom",
            other => bail!("invalid Side for {designator}: {other}"),
        };
        if !designators.insert(designator.to_string()) {
            bail!("duplicate CPL designator '{designator}'");
        }
        let before_x = x;
        let before_y = y;
        let matched = policies.matching_rule(designator, footprint);
        let corrected_angle = if let Some(rule) = matched {
            let (policy_id, rule_id, match_kind, rotation_delta, offset_x, offset_y) =
                rule.values();
            x += offset_x;
            y += offset_y;
            let corrected = if layer == "bottom" {
                (180.0 - (angle - rotation_delta)).rem_euclid(360.0)
            } else {
                (angle + rotation_delta).rem_euclid(360.0)
            };
            applied_corrections.push(JlcpcbAppliedCorrection {
                designator: designator.to_string(),
                footprint: footprint.to_string(),
                side: layer.to_string(),
                policy_id: policy_id.to_string(),
                rule_id: rule_id.to_string(),
                match_kind: match_kind.to_string(),
                rotation_before_degrees: angle,
                rotation_after_degrees: corrected,
                x_before_mm: before_x,
                x_after_mm: x,
                y_before_mm: before_y,
                y_after_mm: y,
            });
            corrected
        } else {
            unmatched_footprints.push(JlcpcbUnmatchedFootprint {
                designator: designator.to_string(),
                footprint: footprint.to_string(),
                side: layer.to_string(),
            });
            if layer == "bottom" {
                (180.0 - angle).rem_euclid(360.0)
            } else {
                angle.rem_euclid(360.0)
            }
        };
        writer.write_record([
            designator.to_string(),
            format!("{x:.6}"),
            format!("{y:.6}"),
            layer.to_string(),
            format!("{corrected_angle:.6}"),
        ])?;
    }
    if designators.is_empty() {
        bail!("KiCad position CSV contains no components");
    }
    writer.flush()?;
    let mut policy_evidence = Vec::new();
    if let Some(project) = &policies.project {
        policy_evidence.push(JlcpcbPolicyEvidence {
            policy_id: project.policy_id.clone(),
            schema_version: project.schema_version,
            provenance: project.provenance.clone(),
            precedence: "project component override, then project footprint first-match",
        });
    }
    policy_evidence.push(JlcpcbPolicyEvidence {
        policy_id: policies.built_in.policy_id.clone(),
        schema_version: policies.built_in.schema_version,
        provenance: policies.built_in.provenance.clone(),
        precedence: "built-in footprint first-match after project policy",
    });
    Ok(JlcpcbCplExport {
        bytes: writer.into_inner()?,
        designators,
        orientation_evidence: JlcpcbOrientationEvidence {
            status: "PREVIEW_REQUIRED",
            physical_validation: false,
            geometry: Vec::new(),
            policies: policy_evidence,
            applied_corrections,
            unmatched_footprints,
            note: "Structural export does not prove physical placement orientation. Inspect every component in JLCPCB Component Placements before ordering.",
        },
    })
}

/// The designators of a JLCPCB BOM, one per reference. The export asks KiCad
/// to list every reference, so a group KiCad still compressed into a range is
/// refused. `known` is the board's CPL designators, when the CPL was exported:
/// a reference that is one of them is a part, even if it looks like a range.
pub(crate) fn jlcpcb_bom_designators(
    source: &str,
    known: Option<&BTreeSet<String>>,
) -> anyhow::Result<BTreeSet<String>> {
    let mut reader = csv::ReaderBuilder::new()
        .trim(csv::Trim::All)
        .from_reader(source.as_bytes());
    let headers = reader.headers()?.clone();
    let reference = headers
        .iter()
        .position(|header| matches!(header, "Designator" | "Reference" | "Refs"))
        .context("JLCPCB BOM has no Designator/Reference/Refs column")?;
    let mut designators = BTreeSet::new();
    for row in reader.records() {
        let row = row?;
        let group = row
            .get(reference)
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .context("JLCPCB BOM contains an empty designator group")?;
        for designator in group.split(',').map(str::trim) {
            // KiCad allows `-` in a reference, so `SMB-19-A1` is one part
            // (#121); only KiCad's own range shape is a compressed group.
            if is_kicad_reference_range(designator)
                && !known.is_some_and(|known| known.contains(designator))
            {
                bail!(
                    "JLCPCB BOM contains compressed designator range '{designator}', although the export asked KiCad to list every reference"
                );
            }
            if designator.is_empty() || !designators.insert(designator.to_string()) {
                bail!("JLCPCB BOM contains an empty or duplicate designator '{designator}'");
            }
        }
    }
    if designators.is_empty() {
        bail!("JLCPCB BOM contains no designators");
    }
    Ok(designators)
}

/// Whether `token` has the shape KiCad gives a compressed group of references:
/// a prefix, a number, `-`, the same prefix and a higher number, such as
/// `C3-C18`. Like KiCad's `SCH_REFERENCE_LIST::Shorthand`, only the trailing
/// number is split off, so the prefix may contain `-` and digits
/// (`SMB-19-A1-SMB-19-A4`).
fn is_kicad_reference_range(token: &str) -> bool {
    fn split_number(reference: &str) -> Option<(&str, u64)> {
        let prefix = reference.trim_end_matches(|c: char| c.is_ascii_digit());
        let number = reference[prefix.len()..].parse().ok()?;
        (!prefix.is_empty()).then_some((prefix, number))
    }
    token.match_indices('-').any(|(at, _)| {
        match (split_number(&token[..at]), split_number(&token[at + 1..])) {
            (Some((first_prefix, first)), Some((last_prefix, last))) => {
                first_prefix == last_prefix && first < last
            }
            _ => false,
        }
    })
}

pub(crate) fn jlcpcb_designator_mismatch(
    cpl: &BTreeSet<String>,
    bom: &BTreeSet<String>,
) -> Option<String> {
    let missing_from_bom = cpl.difference(bom).cloned().collect::<Vec<_>>();
    let missing_from_cpl = bom.difference(cpl).cloned().collect::<Vec<_>>();
    if missing_from_bom.is_empty() && missing_from_cpl.is_empty() {
        None
    } else {
        Some(format!(
            "JLCPCB BOM/CPL designators do not match: missing from BOM [{}]; missing from CPL [{}]",
            missing_from_bom.join(", "),
            missing_from_cpl.join(", ")
        ))
    }
}

#[cfg(test)]
mod jlcpcb_assembly_tests {
    use super::*;

    /// Captured verbatim from KiCad 10.0.6 using:
    /// `kicad-cli pcb export pos --format csv --units mm --side both`
    /// against the repository's real `pic_programmer.kicad_pcb` fixture.
    const KICAD_POSITIONS: &str =
        include_str!("../../../tests/fixtures/positions_pic_programmer_kicad10.csv");
    const ISSUE_518_POSITIONS: &str =
        include_str!("../../../tests/fixtures/positions_jlcpcb_issue518_kicad10.csv");

    fn built_in_policies() -> JlcpcbCorrectionPolicies {
        JlcpcbCorrectionPolicies {
            built_in: parse_jlcpcb_correction_policy(BUILT_IN_JLCPCB_CORRECTIONS, "test").unwrap(),
            project: None,
        }
    }

    fn csv_rows(bytes: &[u8]) -> Vec<Vec<String>> {
        csv::ReaderBuilder::new()
            .has_headers(false)
            .from_reader(bytes)
            .records()
            .map(|record| record.unwrap().iter().map(str::to_string).collect())
            .collect()
    }

    #[test]
    fn real_kicad_positions_become_the_documented_jlcpcb_cpl_schema() {
        let export = jlcpcb_cpl_from_kicad_csv(KICAD_POSITIONS, &built_in_policies()).unwrap();
        let rows = csv_rows(&export.bytes);
        assert_eq!(
            rows[0],
            ["Designator", "Mid X", "Mid Y", "Layer", "Rotation"]
        );
        assert_eq!(
            rows[1],
            ["JP1", "148.082000", "-97.790000", "bottom", "180.000000"]
        );
        assert!(rows.iter().any(|row| {
            row.as_slice() == ["R10", "114.300000", "-48.260000", "top", "0.000000"]
        }));
        assert_eq!(export.designators.len(), rows.len() - 1);
        assert!(export.designators.contains("JP1"));
        assert!(export.designators.contains("R10"));
        assert_eq!(export.orientation_evidence.status, "PREVIEW_REQUIRED");
        assert!(!export.orientation_evidence.physical_validation);
    }

    #[test]
    fn malformed_or_ambiguous_position_rows_fail_instead_of_becoming_cpl() {
        let missing_column = "Ref,Package,PosX,PosY,Rot\nR1,R_0402,1,2,0\n";
        let policies = built_in_policies();
        assert!(jlcpcb_cpl_from_kicad_csv(missing_column, &policies)
            .unwrap_err()
            .to_string()
            .contains("Side"));

        let duplicate =
            "Ref,Package,PosX,PosY,Rot,Side\nR1,R_0402,1,2,0,top\nR1,R_0402,3,4,0,bottom\n";
        assert!(jlcpcb_cpl_from_kicad_csv(duplicate, &policies)
            .unwrap_err()
            .to_string()
            .contains("duplicate CPL designator"));

        let invalid_number = "Ref,Package,PosX,PosY,Rot,Side\nR1,R_0402,left,2,0,top\n";
        assert!(jlcpcb_cpl_from_kicad_csv(invalid_number, &policies)
            .unwrap_err()
            .to_string()
            .contains("invalid PosX"));
    }

    #[test]
    fn native_midpoint_conversion_precedes_explicit_user_offsets() {
        let project = parse_jlcpcb_correction_policy(
            r#"{
            "schema_version":1,"policy_id":"observed-jst-model","provenance":"test",
            "component_overrides":[{"id":"j1","designator":"J1",
            "rotation_degrees":0,"offset_x_mm":3,"offset_y_mm":4}]
        }"#,
            "test",
        )
        .unwrap();
        let policies = JlcpcbCorrectionPolicies {
            built_in: built_in_policies().built_in,
            project: Some(project),
        };
        let midpoints = std::collections::BTreeMap::from([(
            "J1".into(),
            midpoints::Midpoint {
                designator: "J1".into(),
                anchor_x_mm: 15.0,
                anchor_y_mm: 6.0,
                midpoint_x_mm: 16.25,
                midpoint_y_mm: 6.0,
                algorithm: "kicad_native_board_pad_boxes_union_center",
            },
        )]);
        let source = "Ref,Val,Package,PosX,PosY,Rot,Side\nJ1,X,JST,15,-6,0,top\n";
        let (source, geometry) = midpoints::convert_positions(source, &midpoints).unwrap();
        let export = jlcpcb_cpl_from_kicad_csv(&source, &policies).unwrap();
        assert_eq!(
            &csv_rows(&export.bytes)[1][1..],
            ["19.250000", "-2.000000", "top", "0.000000"]
        );
        assert_eq!(geometry[0].anchor_x_mm, 15.0);
        assert_eq!(geometry[0].midpoint_x_mm, 16.25);
    }

    #[test]
    fn built_in_rules_correct_soic_usb_c_bottom_and_normalize_angles() {
        let source = concat!(
            "Ref,Val,Package,PosX,PosY,Rot,Side\n",
            "U1,timer,Package_SO:SOIC-8_3.9x4.9mm_P1.27mm,47,-35,90,top\n",
            "J1,usb,Connector_USB:USB_C_Receptacle_HRO_TYPE-C-31-M-12,30,-26.5,0,top\n",
            "R1,10k,Resistor_SMD:R_0402_1005Metric,1,2,450,top\n",
            "U2,timer,Package_SO:SOIC-8_3.9x4.9mm_P1.27mm,5,6,90,bottom\n",
        );
        let export = jlcpcb_cpl_from_kicad_csv(source, &built_in_policies()).unwrap();
        let rows = csv_rows(&export.bytes);
        assert_eq!(rows[1][4], "0.000000");
        assert_eq!(rows[2][4], "180.000000");
        assert_eq!(rows[3][4], "90.000000");
        assert_eq!(rows[4][4], "0.000000");
        assert_eq!(export.orientation_evidence.applied_corrections.len(), 3);
        assert_eq!(export.orientation_evidence.unmatched_footprints.len(), 1);
        assert_eq!(
            export.orientation_evidence.unmatched_footprints[0].designator,
            "R1"
        );
    }

    #[test]
    fn project_component_override_precedes_project_and_built_in_footprint_rules() {
        let project = parse_jlcpcb_correction_policy(
            r#"{
              "schema_version": 1,
              "policy_id": "project-clock-v1",
              "provenance": "Project owner verified in JLCPCB placement preview",
              "footprint_rules": [{
                "id": "project-soic",
                "footprint_prefix": "SOIC-8_",
                "rotation_degrees": 90,
                "offset_x_mm": 1,
                "offset_y_mm": -2
              }],
              "component_overrides": [{
                "id": "u1-exact",
                "designator": "U1",
                "rotation_degrees": 45,
                "offset_x_mm": 3,
                "offset_y_mm": 4
              }]
            }"#,
            "project test",
        )
        .unwrap();
        let policies = JlcpcbCorrectionPolicies {
            built_in: built_in_policies().built_in,
            project: Some(project),
        };
        let source = concat!(
            "Ref,Val,Package,PosX,PosY,Rot,Side\n",
            "U1,timer,Package_SO:SOIC-8_3.9x4.9mm_P1.27mm,1,2,10,top\n",
            "U2,timer,Package_SO:SOIC-8_3.9x4.9mm_P1.27mm,1,2,10,top\n",
        );
        let export = jlcpcb_cpl_from_kicad_csv(source, &policies).unwrap();
        let rows = csv_rows(&export.bytes);
        assert_eq!(&rows[1][1..], ["4.000000", "6.000000", "top", "55.000000"]);
        assert_eq!(&rows[2][1..], ["2.000000", "0.000000", "top", "100.000000"]);
        assert_eq!(
            export.orientation_evidence.applied_corrections[0].match_kind,
            "component_override"
        );
        assert_eq!(
            export.orientation_evidence.applied_corrections[1].rule_id,
            "project-soic"
        );
    }

    #[test]
    fn malformed_or_ambiguous_correction_policies_fail_closed() {
        let unsupported = r#"{
          "schema_version": 2,
          "policy_id": "future",
          "provenance": "test"
        }"#;
        assert!(parse_jlcpcb_correction_policy(unsupported, "test")
            .unwrap_err()
            .to_string()
            .contains("unsupported schema_version"));

        let duplicate = r#"{
          "schema_version": 1,
          "policy_id": "duplicate",
          "provenance": "test",
          "component_overrides": [
            {"id":"first", "designator":"U1", "rotation_degrees":90},
            {"id":"second", "designator":"U1", "rotation_degrees":180}
          ]
        }"#;
        assert!(parse_jlcpcb_correction_policy(duplicate, "test")
            .unwrap_err()
            .to_string()
            .contains("duplicate override"));
    }

    #[test]
    fn real_issue_518_kicad_fixture_changes_only_known_package_values() {
        let export = jlcpcb_cpl_from_kicad_csv(ISSUE_518_POSITIONS, &built_in_policies()).unwrap();
        let rows = csv_rows(&export.bytes);
        let u1 = rows.iter().find(|row| row[0] == "U1").unwrap();
        let j1 = rows.iter().find(|row| row[0] == "J1").unwrap();
        let r1 = rows.iter().find(|row| row[0] == "R1").unwrap();
        assert_eq!(u1, &["U1", "47.000000", "-35.000000", "top", "0.000000"]);
        assert_eq!(j1, &["J1", "30.000000", "-26.500000", "top", "180.000000"]);
        assert_eq!(r1, &["R1", "38.000000", "-24.000000", "top", "90.000000"]);
        assert_eq!(export.designators.len(), 12);
    }

    #[test]
    fn fully_enumerated_bom_groups_match_individual_cpl_rows() {
        let bom = "\"Designator\",\"Comment\",\"Footprint\",\"LCSC Part #\"\n\"C1,C2,C3\",\"100nF\",\"C_0402\",\"C1525\"\n\"R1\",\"10k\",\"R_0402\",\"C25744\"\n";
        let bom_refs = jlcpcb_bom_designators(bom, None).unwrap();
        let cpl_refs = ["C1", "C2", "C3", "R1"]
            .into_iter()
            .map(str::to_string)
            .collect();
        assert_eq!(jlcpcb_designator_mismatch(&cpl_refs, &bom_refs), None);
    }

    #[test]
    fn compressed_ranges_and_cross_file_mismatches_fail_closed() {
        let ranged = "Designator,Comment\nC1-C3,100nF\n";
        assert!(jlcpcb_bom_designators(ranged, None)
            .unwrap_err()
            .to_string()
            .contains("compressed designator range"));

        let cpl = ["C1", "C2", "U1"].into_iter().map(str::to_string).collect();
        let bom = ["C1", "C2", "R1"].into_iter().map(str::to_string).collect();
        let mismatch = jlcpcb_designator_mismatch(&cpl, &bom).unwrap();
        assert!(mismatch.contains("missing from BOM [U1]"));
        assert!(mismatch.contains("missing from CPL [R1]"));
    }

    /// A reference containing `-` is one part (#121); only KiCad's range
    /// shape is refused, and not when the CPL names it as a part.
    #[test]
    fn hyphenated_references_are_parts_and_only_unknown_ranges_are_refused() {
        let bom = "\"Refs\",\"Value\"\n\"SMB-19-A1,SMB-19-K1\",\"clip\"\n\"R1\",\"10k\"\n\"C1-1\",\"1u\"\n";
        let refs = jlcpcb_bom_designators(bom, None).unwrap();
        assert_eq!(
            refs.into_iter().collect::<Vec<_>>(),
            ["C1-1", "R1", "SMB-19-A1", "SMB-19-K1"]
        );

        let ranged = "Designator,Comment\nC3-C18,100nF\n";
        let error = jlcpcb_bom_designators(ranged, None)
            .unwrap_err()
            .to_string();
        assert!(
            error.contains("compressed designator range 'C3-C18'"),
            "{error}"
        );

        // A reference literally shaped like a range is a part when the CPL
        // has it, and still refused when the CPL is unknown or lacks it.
        let colliding = "Designator,Comment\nC1-C2,odd\n";
        let cpl: BTreeSet<String> = ["C1-C2".to_string()].into();
        assert!(jlcpcb_bom_designators(colliding, Some(&cpl))
            .unwrap()
            .contains("C1-C2"));
        assert!(jlcpcb_bom_designators(colliding, None).is_err());
        let other: BTreeSet<String> = ["C1".to_string()].into();
        assert!(jlcpcb_bom_designators(colliding, Some(&other)).is_err());
    }

    #[test]
    fn kicad_reference_range_shape() {
        for range in [
            "C3-C18",
            "R1-R2",
            "SW-A1-SW-A9",
            "C01-C2",
            // kicad-cli 10.0.6, `sch export bom --group-by Value
            // --ref-range-delimiter '-'`, with references SMB-19-A1..A4.
            "SMB-19-A1-SMB-19-A4",
        ] {
            assert!(is_kicad_reference_range(range), "{range}");
        }
        for reference in [
            "SMB-19-A1",
            "SMB-19-K1",
            "C1-1",
            "R3-R1",
            "R1-C2",
            "C1-C1",
            "-C1",
            "C1-",
            "R1",
            "C-D",
            "1-2",
        ] {
            assert!(!is_kicad_reference_range(reference), "{reference}");
        }
    }

    /// This is an output-level test of KiCad's range switch, not merely an
    /// assertion about the argument vector. The checked-in schematic is a
    /// KiCad demo saved by KiCad and contains groups of one, two, and more than
    /// three identical parts across a hierarchy.
    #[tokio::test]
    #[ignore = "requires an installed KiCad 10 kicad-cli"]
    async fn real_kicad_grouped_bom_enumerates_every_reference() {
        let cli_path = std::env::var("KICAD_CLI_PATH").unwrap_or_else(|_| "kicad-cli".into());
        let schematic = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures/project_ownership/complex_hierarchy.kicad_sch");
        let dir = tempfile::tempdir().unwrap();
        let enumerated = dir.path().join("enumerated.csv");
        let ranged = dir.path().join("ranged.csv");

        let options = cli::BomOptions {
            fields: Some("Reference,Value,Footprint"),
            labels: Some("Designator,Comment,Footprint"),
            group_by: Some("Value,Footprint"),
            exclude_dnp: true,
        };
        cli::export_bom_with_ref_range_delimiter(&cli_path, &schematic, &enumerated, &options, "")
            .await
            .unwrap();
        let source = tokio::fs::read_to_string(&enumerated).await.unwrap();
        let mut reader = csv::Reader::from_reader(source.as_bytes());
        let mut group_sizes = Vec::new();
        for row in reader.records() {
            let row = row.unwrap();
            let group = row.get(0).unwrap();
            assert!(!group.contains('-'), "compressed group escaped: {group}");
            group_sizes.push(group.split(',').count());
        }
        assert!(group_sizes.contains(&1));
        assert!(group_sizes.contains(&2));
        assert!(group_sizes.iter().any(|size| *size >= 3));

        cli::export_bom(&cli_path, &schematic, &ranged, &options)
            .await
            .unwrap();
        let ranged_source = tokio::fs::read_to_string(&ranged).await.unwrap();
        let ranged_refs = jlcpcb_bom_designators(&ranged_source, None).unwrap_err();
        assert!(ranged_refs
            .to_string()
            .contains("compressed designator range"));
    }

    /// The KiCad ECC83 demo contains board footprints excluded from both the
    /// BOM and position files. Running the real exporters proves their output
    /// populations remain identical after the JLCPCB transformations.
    #[tokio::test]
    #[ignore = "requires KiCad 10 kicad-cli and the exact ECC83 demo PCB open with IPC enabled"]
    async fn real_kicad_exclusions_leave_a_matched_bom_and_cpl() {
        let cli_path = std::env::var("KICAD_CLI_PATH").unwrap_or_else(|_| "kicad-cli".into());
        let fixture_root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../copperpour-sexp/tests/fixtures");
        let schematic = fixture_root.join("variants/ecc83-pp.kicad_sch");
        let board = fixture_root.join("ecc83-pp.kicad_pcb");
        let dir = tempfile::tempdir().unwrap();
        let bom = dir.path().join("BOM-ecc83.csv");
        let cpl = dir.path().join("CPL-ecc83.csv");

        let client = copperpour_ipc::KiCadIpcClient::new(
            copperpour_ipc::detect_ipc_address().expect("open the exact demo PCB with IPC enabled"),
        );
        let snapshot =
            midpoints::capture(&client, client.find_open_board(&board).unwrap()).unwrap();
        let staged = dir.path().join("ecc83-pp.kicad_pcb");
        std::fs::write(&staged, &snapshot.contents).unwrap();
        let cpl_export =
            export_jlcpcb_cpl(&cli_path, &staged, &cpl, "both", None, &snapshot.midpoints)
                .await
                .unwrap();
        let options = cli::BomOptions {
            fields: Some("Reference,Value,Footprint"),
            labels: Some("Designator,Comment,Footprint"),
            group_by: Some("Value,Footprint"),
            exclude_dnp: true,
        };
        cli::export_bom_with_ref_range_delimiter(&cli_path, &schematic, &bom, &options, "")
            .await
            .unwrap();
        let bom_source = tokio::fs::read_to_string(&bom).await.unwrap();
        let bom_refs = jlcpcb_bom_designators(&bom_source, Some(&cpl_export.designators)).unwrap();
        assert_eq!(
            jlcpcb_designator_mismatch(&cpl_export.designators, &bom_refs),
            None
        );
    }
}
