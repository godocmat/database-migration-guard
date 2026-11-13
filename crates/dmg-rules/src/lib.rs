//! Shared types and helpers for PostgreSQL migration risk evaluation.
#![forbid(unsafe_code)]

use dmg_core::{
    coverage::Coverage,
    migration::MigrationIdentity,
    risk::{DatabaseObject, Finding, RuleId},
    source::SourceSpan,
};
use serde::Serialize;

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

#[derive(Debug, Clone, Copy, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RelationLock {
    ShareUpdateExclusive,
    Share,
    ShareRowExclusive,
    AccessExclusive,
}

/// Predicted relation lock for a statement, including passing DDL.
#[derive(Debug, Clone, Eq, Ord, PartialEq, PartialOrd, Serialize)]
pub struct RelationLockObservation {
    pub migration: MigrationIdentity,
    pub span: SourceSpan,
    pub object: DatabaseObject,
    pub mode: RelationLock,
}

pub fn supports_rule(id: RuleId) -> bool {
    Rule::ALL.iter().any(|rule| rule.id() == id)
}
