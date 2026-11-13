//! Versioned policy configuration and decision types.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::{
    digest::ContentDigest,
    risk::{RuleId, Severity},
    schema::SchemaVersionV1,
};

pub const POLICY_SCHEMA_VERSION: u32 = SchemaVersionV1::NUMBER;

#[derive(Debug, Clone, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PolicyV1 {
    schema_version: SchemaVersionV1,
    #[serde(default = "default_deny_at")]
    pub deny_at: Severity,
    #[serde(default)]
    pub integrity: IntegrityPolicy,
    #[serde(default)]
    pub rules: BTreeMap<RuleId, RulePolicy>,
}

impl Default for PolicyV1 {
    fn default() -> Self {
        Self {
            schema_version: SchemaVersionV1,
            deny_at: default_deny_at(),
            integrity: IntegrityPolicy::default(),
            rules: BTreeMap::new(),
        }
    }
}

impl PolicyV1 {
    /// Reads the version and the TOML shape. Registry and decision checks use the evaluated ruleset.
    pub fn from_toml(input: &str) -> Result<Self, PolicyFormatError> {
        Ok(toml::from_str(input)?)
    }
}

fn default_deny_at() -> Severity {
    Severity::High
}

#[derive(Debug, Clone, Eq, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct IntegrityPolicy {
    pub require_git_base: bool,
    pub deny_modified: bool,
    pub deny_missing: bool,
}

impl Default for IntegrityPolicy {
    fn default() -> Self {
        Self {
            require_git_base: false,
            deny_modified: true,
            deny_missing: true,
        }
    }
}

#[derive(Debug, Clone, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RulePolicy {
    pub severity: Severity,
}

#[derive(Debug, Error)]
#[error("invalid versioned policy TOML")]
pub struct PolicyFormatError(#[from] pub toml::de::Error);

#[derive(Debug, Clone, Eq, PartialEq, Serialize, Deserialize)]
pub struct PolicyDecision {
    pub status: DecisionStatus,
    pub policy_digest: ContentDigest,
    pub reasons: Vec<DecisionReason>,
}

#[derive(Debug, Clone, Copy, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DecisionStatus {
    Pass,
    Denied,
    Incomplete,
}

#[derive(Debug, Clone, Eq, PartialEq, Ord, PartialOrd, Serialize, Deserialize)]
pub struct DecisionReason {
    pub kind: DecisionReasonKind,
    pub message: String,
    pub finding_fingerprint: Option<ContentDigest>,
}

#[derive(Debug, Clone, Copy, Eq, PartialEq, Ord, PartialOrd, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DecisionReasonKind {
    SeverityThreshold,
    IntegrityViolation,
    RequiredCoverage,
    RequiredEvidence,
}
