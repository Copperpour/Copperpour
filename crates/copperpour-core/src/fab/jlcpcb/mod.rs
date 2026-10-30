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
    /// Footprint-name prefixes whose rotation does not matter (two-terminal
    /// unpolarized parts). Unmatched components outside these prefixes are
    /// reported for review instead of passing silently.
    #[serde(default)]
    rotation_insensitive_prefixes: Vec<String>,
    #[serde(default)]
    footprint_rules: Vec<JlcpcbCorrectionRule>,
    #[serde(default)]
    component_overrides: Vec<JlcpcbComponentOverride>,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct JlcpcbCorrectionRule {
    id: String,
    #[serde(default)]
    footprint_prefix: Option<String>,
    #[serde(default)]
    footprint_exact: Option<String>,
    #[serde(default)]
    status: JlcpcbRuleStatus,
    #[serde(default)]
    rotation_degrees: f64,
    #[serde(default)]
    offset_x_mm: f64,
    #[serde(default)]
    offset_y_mm: f64,
    #[serde(default)]
    evidence: Vec<JlcpcbRotationObservation>,
    #[serde(default)]
    note: Option<String>,
}

impl JlcpcbCorrectionRule {
    fn matches(&self, footprint: &str) -> bool {
        match (&self.footprint_exact, &self.footprint_prefix) {
            (Some(exact), _) => footprint == exact,
            (None, Some(prefix)) => footprint.starts_with(prefix.as_str()),
            (None, None) => false,
        }
    }
}

/// An unverified rule names a footprint family whose JLCPCB orientation has
/// not been observed: it applies no correction and turns every match into a
/// review warning.
#[derive(Clone, Copy, Debug, Default, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
enum JlcpcbRuleStatus {
    #[default]
    Verified,
    Unverified,
}

/// Direction of pin 1 from the part centre, as seen in JLCPCB Component
/// Placements (top view, board coordinates: up is +Y).
#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
enum PinDirection {
    Right,
    TopRight,
    Top,
    TopLeft,
    Left,
    BottomLeft,
    Bottom,
    BottomRight,
}

impl PinDirection {
    /// Counter-clockwise angle from +X, the sense of KiCad and CPL rotations.
    fn degrees(self) -> f64 {
        match self {
            Self::Right => 0.0,
            Self::TopRight => 45.0,
            Self::Top => 90.0,
            Self::TopLeft => 135.0,
            Self::Left => 180.0,
            Self::BottomLeft => 225.0,
            Self::Bottom => 270.0,
            Self::BottomRight => 315.0,
        }
    }
}

/// One look at the JLCPCB placement preview: where pin 1 of the KiCad
/// footprint and pin 1 of the JLCPCB model landed, for a CPL rotation that
/// was actually uploaded.
#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct JlcpcbRotationObservation {
    board: String,
    designator: String,
    #[serde(default)]
    footprint: Option<String>,
    kicad_rotation_degrees: f64,
    cpl_rotation_degrees: f64,
    footprint_pin1: PinDirection,
    jlc_model_pin1: PinDirection,
}

impl JlcpcbRotationObservation {
    /// The correction this observation proves: the model's pin 1 at CPL
    /// rotation 0 is `jlc_model_pin1 - cpl`, and it must reach
    /// `footprint_pin1` at `kicad + correction`.
    fn implied_rotation_degrees(&self) -> f64 {
        (self.footprint_pin1.degrees() - self.jlc_model_pin1.degrees() + self.cpl_rotation_degrees
            - self.kicad_rotation_degrees)
            .rem_euclid(360.0)
    }
}

fn same_angle(a: f64, b: f64) -> bool {
    let difference = (a - b).rem_euclid(360.0);
    difference.min(360.0 - difference) < 0.5
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
    #[serde(default)]
    evidence: Vec<JlcpcbRotationObservation>,
    #[serde(default)]
    note: Option<String>,
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
                .find(|rule| rule.matches(footprint))
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
            .find(|rule| rule.matches(footprint))
            .map(|rule| MatchedRule::Footprint {
                policy: &self.built_in,
                rule,
            })
    }

    fn rotation_insensitive(&self, footprint: &str) -> bool {
        self.project
            .iter()
            .chain(std::iter::once(&self.built_in))
            .flat_map(|policy| &policy.rotation_insensitive_prefixes)
            .any(|prefix| footprint.starts_with(prefix.as_str()))
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
    fn unverified(&self) -> Option<(&str, &str, Option<&str>)> {
        match self {
            Self::Footprint { policy, rule } if rule.status == JlcpcbRuleStatus::Unverified => {
                Some((&policy.policy_id, &rule.id, rule.note.as_deref()))
            }
            _ => None,
        }
    }

    fn note(&self) -> Option<&str> {
        match self {
            Self::Component { rule, .. } => rule.note.as_deref(),
            Self::Footprint { rule, .. } => rule.note.as_deref(),
        }
    }

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
    if policy
        .rotation_insensitive_prefixes
        .iter()
        .any(|prefix| prefix.trim().is_empty())
    {
        bail!("JLCPCB correction policy {description} has an empty rotation_insensitive_prefixes entry");
    }
    let mut ids = BTreeSet::new();
    for rule in &policy.footprint_rules {
        let pattern = match (&rule.footprint_prefix, &rule.footprint_exact) {
            (Some(pattern), None) | (None, Some(pattern)) => pattern,
            _ => bail!(
                "JLCPCB correction rule '{}' in {description} needs exactly one of footprint_prefix or footprint_exact",
                rule.id
            ),
        };
        if rule.id.trim().is_empty() || pattern.trim().is_empty() {
            bail!(
                "JLCPCB correction policy {description} has an empty rule id or footprint pattern"
            );
        }
        if rule.status == JlcpcbRuleStatus::Unverified
            && [rule.rotation_degrees, rule.offset_x_mm, rule.offset_y_mm]
                .iter()
                .any(|value| *value != 0.0)
        {
            bail!(
                "JLCPCB correction rule '{}' in {description} is unverified and cannot carry a correction",
                rule.id
            );
        }
        if rule.status == JlcpcbRuleStatus::Verified {
            validate_observations(description, &rule.id, rule.rotation_degrees, &rule.evidence)?;
        }
        if let Some(footprint) = rule
            .evidence
            .iter()
            .filter_map(|observation| observation.footprint.as_deref())
            .find(|footprint| !rule.matches(footprint))
        {
            bail!(
                "JLCPCB correction rule '{}' in {description} cites an observation of {footprint}, which it does not match",
                rule.id
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
        validate_observations(description, &rule.id, rule.rotation_degrees, &rule.evidence)?;
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

/// A rule whose own observations contradict its rotation is the J201 failure
/// (an HRO USB-C turned 180 degrees from a correct KiCad rotation): refuse it
/// before it reaches a CPL.
fn validate_observations(
    description: &str,
    id: &str,
    rotation: f64,
    evidence: &[JlcpcbRotationObservation],
) -> anyhow::Result<()> {
    for observation in evidence {
        let implied = observation.implied_rotation_degrees();
        if !same_angle(implied, rotation) {
            bail!(
                "JLCPCB correction rule '{id}' in {description} rotates by {rotation} degrees, but its observation of {} on {} implies {implied} degrees",
                observation.designator,
                observation.board
            );
        }
    }
    Ok(())
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
    unverified_footprints: Vec<JlcpcbUnverifiedFootprint>,
    note: &'static str,
}

impl JlcpcbOrientationEvidence {
    /// Components left at KiCad's raw rotation whose orientation matters:
    /// one warning per unverified rule, one for every polarized or multi-pin
    /// footprint that no rule covers. An empty result is the only state in
    /// which the package may say it has no warnings.
    pub(crate) fn review_warnings(&self) -> Vec<String> {
        let mut warnings = Vec::new();
        let mut unverified = std::collections::BTreeMap::<(&str, &str, &str), Vec<&str>>::new();
        for part in &self.unverified_footprints {
            unverified
                .entry((
                    &part.rule_id,
                    &part.footprint,
                    part.note.as_deref().unwrap_or(""),
                ))
                .or_default()
                .push(&part.designator);
        }
        for ((rule_id, footprint, note), designators) in unverified {
            let mut warning = format!(
                "JLCPCB rotation not verified for footprint {footprint} (rule {rule_id}); left at KiCad rotation: {}.",
                designators.join(", ")
            );
            if !note.is_empty() {
                warning.push(' ');
                warning.push_str(note);
            }
            warnings.push(warning);
        }
        let mut uncovered = std::collections::BTreeMap::<&str, Vec<&str>>::new();
        for part in self
            .unmatched_footprints
            .iter()
            .filter(|part| part.review_required)
        {
            uncovered
                .entry(&part.footprint)
                .or_default()
                .push(&part.designator);
        }
        if !uncovered.is_empty() {
            warnings.push(format!(
                "No JLCPCB rotation rule for {} polarized or multi-pin footprint(s), left at KiCad rotation; check pin 1 in Component Placements: {}.",
                uncovered.len(),
                uncovered
                    .iter()
                    .map(|(footprint, designators)| format!("{footprint} ({})", designators.join(", ")))
                    .collect::<Vec<_>>()
                    .join("; ")
            ));
        }
        warnings
    }
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
    #[serde(skip_serializing_if = "Option::is_none")]
    note: Option<String>,
}

#[derive(Clone, Debug, Serialize)]
struct JlcpcbUnmatchedFootprint {
    designator: String,
    footprint: String,
    side: String,
    review_required: bool,
}

#[derive(Clone, Debug, Serialize)]
struct JlcpcbUnverifiedFootprint {
    designator: String,
    footprint: String,
    side: String,
    policy_id: String,
    rule_id: String,
    note: Option<String>,
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
    let mut unverified_footprints = Vec::new();
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
        let unverified = matched.as_ref().and_then(MatchedRule::unverified);
        let is_unverified = unverified.is_some();
        if let Some((policy_id, rule_id, note)) = unverified {
            unverified_footprints.push(JlcpcbUnverifiedFootprint {
                designator: designator.to_string(),
                footprint: footprint.to_string(),
                side: layer.to_string(),
                policy_id: policy_id.to_string(),
                rule_id: rule_id.to_string(),
                note: note.map(str::to_string),
            });
        }
        let corrected_angle = if let Some(rule) = matched.filter(|_| !is_unverified) {
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
                note: rule.note().map(str::to_string),
            });
            corrected
        } else {
            if !is_unverified {
                unmatched_footprints.push(JlcpcbUnmatchedFootprint {
                    designator: designator.to_string(),
                    footprint: footprint.to_string(),
                    side: layer.to_string(),
                    review_required: !policies.rotation_insensitive(footprint),
                });
            }
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
            unverified_footprints,
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
    fn built_in_rules_correct_soic_on_both_sides_and_leave_usb_c_for_review() {
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
        // Same-named HRO footprints disagreed in the preview: KiCad's rotation
        // passes through untouched and the part is named for review.
        assert_eq!(rows[2][4], "0.000000");
        assert_eq!(rows[3][4], "90.000000");
        assert_eq!(rows[4][4], "0.000000");
        let evidence = &export.orientation_evidence;
        assert_eq!(evidence.applied_corrections.len(), 2);
        assert_eq!(evidence.unverified_footprints.len(), 1);
        assert_eq!(evidence.unverified_footprints[0].designator, "J1");
        assert_eq!(evidence.unmatched_footprints.len(), 1);
        assert_eq!(evidence.unmatched_footprints[0].designator, "R1");
        assert!(!evidence.unmatched_footprints[0].review_required);
        let warnings = evidence.review_warnings();
        assert_eq!(warnings.len(), 1);
        assert!(warnings[0].starts_with(
            "JLCPCB rotation not verified for footprint USB_C_Receptacle_HRO_TYPE-C-31-M-12"
        ));
        assert!(warnings[0].contains(": J1."));
    }

    /// The regression bench: every built-in rule must be backed by at least
    /// one preview observation, and each observation must imply exactly the
    /// rule's rotation (pin 1 of the KiCad footprint, turned by KiCad, lands
    /// where pin 1 of the JLCPCB model lands once the CPL rotation is applied).
    #[test]
    fn every_built_in_rule_agrees_with_its_jlcpcb_preview_observations() {
        let policy = built_in_policies().built_in;
        assert!(!policy.footprint_rules.is_empty());
        for rule in &policy.footprint_rules {
            match rule.status {
                JlcpcbRuleStatus::Verified => {
                    assert!(
                        !rule.evidence.is_empty(),
                        "built-in rule {} has no preview observation",
                        rule.id
                    );
                    for observation in &rule.evidence {
                        assert!(
                            same_angle(
                                observation.implied_rotation_degrees(),
                                rule.rotation_degrees
                            ),
                            "rule {} vs {}",
                            rule.id,
                            observation.designator
                        );
                        assert!(observation.footprint.is_some(), "rule {}", rule.id);
                    }
                }
                JlcpcbRuleStatus::Unverified => {
                    assert!(
                        rule.note.is_some(),
                        "unverified rule {} has no note",
                        rule.id
                    );
                }
            }
        }
    }

    #[test]
    fn observations_compute_the_correction_from_pin_one_positions() {
        let observation = |kicad, cpl, footprint_pin1, jlc_model_pin1| JlcpcbRotationObservation {
            board: "test".into(),
            designator: "U1".into(),
            footprint: None,
            kicad_rotation_degrees: kicad,
            cpl_rotation_degrees: cpl,
            footprint_pin1,
            jlc_model_pin1,
        };
        use PinDirection::*;
        // Upright KiCad SOIC, JLCPCB model lying down: +270.
        assert_eq!(
            observation(0.0, 0.0, TopLeft, BottomLeft).implied_rotation_degrees(),
            270.0
        );
        // SOT-23, model pin 1 diagonally opposite: +180.
        assert_eq!(
            observation(0.0, 0.0, TopLeft, BottomRight).implied_rotation_degrees(),
            180.0
        );
        // Issue #518 after its correction: aligned at CPL 0 for KiCad 90.
        assert_eq!(
            observation(90.0, 0.0, BottomLeft, BottomLeft).implied_rotation_degrees(),
            270.0
        );
        // grumpf-bord J201: KiCad 180 was right, the 180 rule flipped it.
        assert_eq!(
            observation(180.0, 0.0, BottomRight, TopLeft).implied_rotation_degrees(),
            0.0
        );
    }

    #[test]
    fn a_rule_contradicted_by_its_own_observation_is_refused() {
        // The rule that turned grumpf-bord J201 upside down, with the
        // observation that caught it.
        let contradicted = r#"{
          "schema_version": 1,
          "policy_id": "old-usb-c",
          "provenance": "test",
          "footprint_rules": [{
            "id": "kicad-hro-type-c-31-m-12",
            "footprint_prefix": "USB_C_Receptacle_HRO_TYPE-C-31-M-12",
            "rotation_degrees": 180,
            "evidence": [{
              "board": "grumpf-bord", "designator": "J201",
              "kicad_rotation_degrees": 180, "cpl_rotation_degrees": 0,
              "footprint_pin1": "bottom-right", "jlc_model_pin1": "top-left"
            }]
          }]
        }"#;
        let error = parse_jlcpcb_correction_policy(contradicted, "test")
            .unwrap_err()
            .to_string();
        assert!(error.contains("implies 0 degrees"), "{error}");
    }

    #[test]
    fn unverified_and_ambiguous_rules_fail_closed() {
        let unverified_with_rotation = r#"{
          "schema_version": 1, "policy_id": "p", "provenance": "test",
          "footprint_rules": [{"id": "msop", "footprint_prefix": "MSOP-",
            "status": "unverified", "rotation_degrees": 270}]
        }"#;
        assert!(
            parse_jlcpcb_correction_policy(unverified_with_rotation, "test")
                .unwrap_err()
                .to_string()
                .contains("cannot carry a correction")
        );
        for both_or_neither in [
            r#""footprint_prefix": "SOT-23", "footprint_exact": "SOT-23""#,
            r#""rotation_degrees": 180"#,
        ] {
            let policy = format!(
                r#"{{"schema_version": 1, "policy_id": "p", "provenance": "test",
                  "footprint_rules": [{{"id": "sot", {both_or_neither}}}]}}"#
            );
            assert!(parse_jlcpcb_correction_policy(&policy, "test")
                .unwrap_err()
                .to_string()
                .contains("exactly one of footprint_prefix or footprint_exact"));
        }
    }

    #[test]
    fn sot_23_variants_do_not_share_a_correction() {
        let source = concat!(
            "Ref,Val,Package,PosX,PosY,Rot,Side\n",
            "Q1,npn,Package_TO_SOT_SMD:SOT-23,1,2,0,top\n",
            "U1,ldo,Package_TO_SOT_SMD:SOT-23-5,1,2,0,top\n",
            "U2,mux,Package_TO_SOT_SMD:SOT-23-6,1,2,0,top\n",
        );
        let export = jlcpcb_cpl_from_kicad_csv(source, &built_in_policies()).unwrap();
        let rows = csv_rows(&export.bytes);
        // Three packages, three JLCPCB models: the exact names keep SOT-23's
        // +180 from reaching the 5- and 6-pin variants.
        assert_eq!(rows[1][4], "180.000000");
        assert_eq!(rows[2][4], "180.000000");
        assert_eq!(rows[3][4], "270.000000");
        let rules = &export.orientation_evidence.applied_corrections;
        assert_eq!(rules[1].rule_id, "kicad-sot-23-5");
        assert_eq!(rules[2].rule_id, "kicad-sot-23-6");
    }

    #[test]
    fn uncovered_polarized_parts_are_listed_and_passives_are_not() {
        let source = concat!(
            "Ref,Val,Package,PosX,PosY,Rot,Side\n",
            "R1,10k,Resistor_SMD:R_0402_1005Metric,1,2,0,top\n",
            "C1,1u,Capacitor_SMD:C_0603_1608Metric,1,2,0,top\n",
            "U1,ldo,Package_TO_SOT_SMD:SOT-223-3_TabPin2,1,2,0,top\n",
            "U2,ldo,Package_TO_SOT_SMD:SOT-223-3_TabPin2,1,2,90,top\n",
            "K1,relay,Relay_THT:Relay_SPDT_Finder_36.11,1,2,0,top\n",
        );
        let export = jlcpcb_cpl_from_kicad_csv(source, &built_in_policies()).unwrap();
        let warnings = export.orientation_evidence.review_warnings();
        assert_eq!(warnings.len(), 1);
        assert!(warnings[0].contains("for 2 polarized or multi-pin footprint(s)"));
        assert!(warnings[0].contains("SOT-223-3_TabPin2 (U1, U2)"));
        assert!(warnings[0].contains("Relay_SPDT_Finder_36.11 (K1)"));
        assert!(!warnings[0].contains("R1"));

        let passives_only =
            "Ref,Val,Package,PosX,PosY,Rot,Side\nR1,10k,R_0402_1005Metric,1,2,0,top\n";
        let export = jlcpcb_cpl_from_kicad_csv(passives_only, &built_in_policies()).unwrap();
        assert!(export.orientation_evidence.review_warnings().is_empty());
    }

    #[test]
    fn documented_project_policy_example_pins_designators() {
        let project = parse_jlcpcb_correction_policy(
            include_str!("../../../../../examples/jlcpcb_cpl_corrections.example.json"),
            "example",
        )
        .unwrap();
        let policies = JlcpcbCorrectionPolicies {
            built_in: built_in_policies().built_in,
            project: Some(project),
        };
        let source = concat!(
            "Ref,Val,Package,PosX,PosY,Rot,Side\n",
            "J201,usb,Connector_USB:USB_C_Receptacle_HRO_TYPE-C-31-M-12,1,2,180,top\n",
            "U402,amp,Package_SO:TSSOP-8_4.4x3mm_P0.65mm,1,2,0,top\n",
        );
        let export = jlcpcb_cpl_from_kicad_csv(source, &policies).unwrap();
        let rows = csv_rows(&export.bytes);
        assert_eq!(rows[1][4], "180.000000");
        assert_eq!(rows[2][4], "270.000000");
        assert!(export.orientation_evidence.review_warnings().is_empty());
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
        // The #518 J1 correction is no longer built in (see the HRO rule).
        assert_eq!(j1, &["J1", "30.000000", "-26.500000", "top", "0.000000"]);
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
