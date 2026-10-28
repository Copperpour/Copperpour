//! Shared plan-applicability contract for placement planners that can refuse
//! to apply an unsafe or useless plan rather than silently executing it.
//!
//! A planner accumulates blocking reasons as it checks a plan (out-of-bounds
//! targets, a score that does not improve, …); `status()` reports
//! `"applicable"` only when none were found. The same shape is meant for
//! every planner that can produce a plan a caller should not blindly apply
//! (decoupling placement and deprecated force-directed refinement) so callers
//! learn one contract, not one per tool.

use serde_json::{json, Value};

/// Accumulated blocking reasons for one planned mutation. Empty means the
/// plan is safe to apply as evaluated.
#[derive(Debug, Default, Clone)]
pub struct PlanApplicability {
    blocking_reasons: Vec<String>,
}

impl PlanApplicability {
    pub fn new() -> Self {
        Self::default()
    }

    /// Record a reason the plan must not be applied as-is.
    pub fn block(&mut self, reason: impl Into<String>) {
        self.blocking_reasons.push(reason.into());
    }

    pub fn is_blocked(&self) -> bool {
        !self.blocking_reasons.is_empty()
    }

    pub fn status_str(&self) -> &'static str {
        if self.is_blocked() {
            "blocked"
        } else {
            "applicable"
        }
    }

    pub fn blocking_reasons(&self) -> &[String] {
        &self.blocking_reasons
    }

    /// The `plan_status` / `blocking_reasons` pair, ready to splice into a
    /// tool's JSON response ahead of `planned_moves` / `applied`.
    pub fn to_json(&self) -> (Value, Value) {
        (json!(self.status_str()), json!(self.blocking_reasons))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn no_blocks_is_applicable() {
        let applicability = PlanApplicability::new();
        assert!(!applicability.is_blocked());
        assert_eq!(applicability.status_str(), "applicable");
        assert!(applicability.blocking_reasons().is_empty());
    }

    #[test]
    fn any_block_is_blocked_and_named() {
        let mut applicability = PlanApplicability::new();
        applicability.block("C1 plans outside the board outline");
        assert!(applicability.is_blocked());
        assert_eq!(applicability.status_str(), "blocked");
        assert_eq!(
            applicability.blocking_reasons(),
            &["C1 plans outside the board outline".to_string()]
        );
    }
}
