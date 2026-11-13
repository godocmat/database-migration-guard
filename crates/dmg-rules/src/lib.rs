//! Shared types and helpers for PostgreSQL migration risk evaluation.
#![forbid(unsafe_code)]

use dmg_core::{
    coverage::Coverage,
    risk::{Finding, RuleId},
    target::PostgresTarget,
};
use dmg_postgres::ParsedPlan;

pub use dmg_core::risk::{RelationLock, RelationLockObservation};

mod ddl;
mod dml;
mod finding;
mod schema;
mod transaction;
pub use finding::Rule;

pub const RULESET_VERSION: &str = "1";

#[derive(Debug, Clone, Eq, PartialEq)]
pub struct RuleOptions {
    /// Trusted search path for resolving names in a static migration inventory.
    pub search_path: Vec<String>,
}

impl Default for RuleOptions {
    fn default() -> Self {
        Self {
            search_path: vec![String::from("public")],
        }
    }
}

#[derive(Debug, Clone, Eq, PartialEq)]
pub struct Analysis {
    pub coverage: Coverage,
    pub findings: Vec<Finding>,
    pub locks: Vec<RelationLockObservation>,
}

pub fn supports_rule(id: RuleId) -> bool {
    Rule::ALL.iter().any(|rule| rule.id() == id)
}

/// Evaluate the supported rule families in discovery order with original source locations.
pub fn analyze(plan: &ParsedPlan, target: PostgresTarget, options: &RuleOptions) -> Analysis {
    let mut findings = Vec::new();
    let mut gaps = plan.coverage.gaps().to_vec();
    let mut locks = Vec::new();
    let timelines = transaction::timelines(plan);
    ddl::evaluate(
        plan,
        target,
        options,
        &timelines,
        &mut findings,
        &mut gaps,
        &mut locks,
    );
    dml::evaluate(plan, options, &timelines, &mut findings, &mut gaps);
    for finding in &mut findings {
        finding.evidence.sort();
        finding.evidence.dedup();
    }
    findings.sort();
    findings.dedup();
    locks.sort();
    locks.dedup();

    Analysis {
        coverage: Coverage::from_gaps(gaps),
        findings,
        locks,
    }
}
