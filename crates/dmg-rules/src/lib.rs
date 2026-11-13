//! Shared types and helpers for PostgreSQL migration risk evaluation.
#![forbid(unsafe_code)]

use dmg_core::{
    coverage::Coverage,
    risk::{Finding, RuleId},
};

pub use dmg_core::risk::{RelationLock, RelationLockObservation};

#[allow(dead_code)] // Helpers gain their consumers when the two rule assignments land.
mod finding;
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
