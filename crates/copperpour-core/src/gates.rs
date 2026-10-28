//! The gate/verdict contract shared by every readiness check.
//!
//! A gate is a named check whose outcome must rest on evidence. The central
//! invariant, learned the hard way across #218/#244/#247: **a check that did
//! not run can never read as a pass**. A skipped or unrunnable check becomes
//! [`GateStatus::Blocked`], and `Blocked` outranks `Fail` in the composite —
//! "we could not evaluate this design" is a stronger statement about
//! readiness than "we evaluated it and found problems", because the latter at
//! least bounds what is wrong.
//!
//! The composite verdict maps onto Konnect's existing release vocabulary:
//! anything other than `Pass` composes to the "NOT READY" / "INCOMPLETE"
//! family — the exact strings stay with the tools that own them
//! (`validate_for_manufacturing`, `run_design_review`); this module only
//! decides the ordering.

use serde::{Deserialize, Serialize};

/// Outcome of one gate check.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "UPPERCASE")]
pub enum GateStatus {
    /// The check ran and the design satisfies it.
    Pass,
    /// The check ran and found advisory problems.
    Warn,
    /// The check ran and found disqualifying problems.
    Fail,
    /// The check could not run — skipped by request, a prerequisite missing,
    /// or its evidence incomplete. Never a pass.
    Blocked,
    /// There was nothing to evaluate (an empty project). Dominates
    /// everything: no verdict about an empty design is meaningful.
    Empty,
}

impl GateStatus {
    /// Precedence for composition; higher dominates.
    fn rank(self) -> u8 {
        match self {
            GateStatus::Pass => 0,
            GateStatus::Warn => 1,
            GateStatus::Fail => 2,
            GateStatus::Blocked => 3,
            GateStatus::Empty => 4,
        }
    }
}

/// Fold a set of gate outcomes into one composite status.
///
/// Precedence: `Empty > Blocked > Fail > Warn > Pass`. An empty iterator is
/// `Empty` — zero checks is not evidence of anything.
pub fn combined_status<I>(outcomes: I) -> GateStatus
where
    I: IntoIterator<Item = GateStatus>,
{
    outcomes
        .into_iter()
        .max_by_key(|status| status.rank())
        .unwrap_or(GateStatus::Empty)
}

/// Classify a measured value against a pass ceiling and a warn ceiling.
///
/// `value <= pass_max` is `Pass`, `value <= warn_max` is `Warn`, above is
/// `Fail`. Callers with no independent warn ceiling use
/// [`warn_max_from`]'s default of twice the pass ceiling — a gate that can
/// only return `Pass` or `Warn` is not a gate.
pub fn three_level_verdict(value: f64, pass_max: f64, warn_max: f64) -> GateStatus {
    if value <= pass_max {
        GateStatus::Pass
    } else if value <= warn_max {
        GateStatus::Warn
    } else {
        GateStatus::Fail
    }
}

/// The default warn ceiling: twice the pass ceiling.
pub fn warn_max_from(pass_max: f64) -> f64 {
    pass_max * 2.0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn precedence_is_empty_blocked_fail_warn_pass() {
        use GateStatus::*;
        assert_eq!(combined_status([Pass, Pass]), Pass);
        assert_eq!(combined_status([Pass, Warn]), Warn);
        assert_eq!(combined_status([Warn, Fail, Pass]), Fail);
        assert_eq!(
            combined_status([Fail, Blocked]),
            Blocked,
            "unevaluable outranks failed"
        );
        assert_eq!(combined_status([Blocked, Empty, Fail]), Empty);
    }

    #[test]
    fn zero_checks_is_empty_not_pass() {
        assert_eq!(combined_status([]), GateStatus::Empty);
    }

    /// The load-bearing property: no set containing a non-Pass member may
    /// compose to Pass. This is the fail-closed rule in one assertion.
    #[test]
    fn any_non_pass_member_prevents_a_pass_composite() {
        use GateStatus::*;
        for non_pass in [Warn, Fail, Blocked, Empty] {
            for position in 0..3 {
                let mut outcomes = [Pass, Pass, Pass];
                outcomes[position] = non_pass;
                assert_ne!(
                    combined_status(outcomes.iter().copied()),
                    Pass,
                    "{non_pass:?} at index {position} must not compose to Pass"
                );
            }
        }
    }

    #[test]
    fn three_level_boundaries_are_inclusive() {
        assert_eq!(three_level_verdict(10.0, 10.0, 20.0), GateStatus::Pass);
        assert_eq!(three_level_verdict(10.001, 10.0, 20.0), GateStatus::Warn);
        assert_eq!(three_level_verdict(20.0, 10.0, 20.0), GateStatus::Warn);
        assert_eq!(three_level_verdict(20.001, 10.0, 20.0), GateStatus::Fail);
    }

    #[test]
    fn warn_default_is_twice_pass() {
        assert_eq!(warn_max_from(10.0), 20.0);
    }

    #[test]
    fn statuses_serialize_uppercase_for_reports() {
        assert_eq!(
            serde_json::to_string(&GateStatus::Blocked).unwrap(),
            "\"BLOCKED\""
        );
    }
}
