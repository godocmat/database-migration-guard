//! Stable finding and risk vocabulary shared by rules, policy, and reports.

use std::{fmt, str::FromStr};

use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::{digest::ContentDigest, migration::MigrationIdentity, source::SourceSpan};

#[derive(Debug, Clone, Copy, Eq, PartialEq, Ord, PartialOrd, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Severity {
    Info,
    Low,
    Medium,
    High,
    Critical,
}

#[derive(Debug, Clone, Copy, Eq, PartialEq, Ord, PartialOrd, Hash, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct RuleId(u16);

impl FromStr for RuleId {
    type Err = InvalidRuleId;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        let number = value.strip_prefix("PG").ok_or(InvalidRuleId)?;
        if number.len() != 3 || !number.bytes().all(|byte| byte.is_ascii_digit()) {
            return Err(InvalidRuleId);
        }

        let number = number.parse().map_err(|_| InvalidRuleId)?;
        if number == 0 {
            return Err(InvalidRuleId);
        }

        Ok(Self(number))
    }
}

impl TryFrom<String> for RuleId {
    type Error = InvalidRuleId;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        value.parse()
    }
}

impl From<RuleId> for String {
    fn from(id: RuleId) -> Self {
        id.to_string()
    }
}

impl fmt::Display for RuleId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "PG{:03}", self.0)
    }
}

#[derive(Debug, Error, Eq, PartialEq)]
#[error("expected a rule ID in the range PG001 through PG999")]
pub struct InvalidRuleId;

#[derive(Debug, Clone, Eq, PartialEq, Ord, PartialOrd, Serialize, Deserialize)]
pub struct Finding {
    pub span: SourceSpan,
    pub rule_id: RuleId,
    pub migration: MigrationIdentity,
    pub object: Option<DatabaseObject>,
    pub fingerprint: ContentDigest,
    pub severity: Severity,
    pub confidence: Confidence,
    pub explanation: String,
    pub impact: String,
    pub mitigation: String,
    pub documentation: String,
    pub evidence: Vec<FindingEvidence>,
}

#[derive(Debug, Clone, Eq, PartialEq, Ord, PartialOrd, Serialize, Deserialize)]
pub struct DatabaseObject {
    pub kind: ObjectKind,
    pub schema: Option<String>,
    pub name: String,
    pub member: Option<String>,
}

#[derive(Debug, Clone, Copy, Eq, PartialEq, Ord, PartialOrd, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ObjectKind {
    Schema,
    Relation,
    Column,
    Index,
    Constraint,
    Type,
}

#[derive(Debug, Clone, Copy, Eq, PartialEq, Ord, PartialOrd, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Confidence {
    Low,
    Medium,
    High,
}

#[derive(Debug, Clone, Eq, PartialEq, Ord, PartialOrd, Serialize, Deserialize)]
pub struct FindingEvidence {
    pub kind: EvidenceKind,
    pub detail: String,
    pub artifact_digest: Option<ContentDigest>,
}

#[derive(Debug, Clone, Copy, Eq, PartialEq, Ord, PartialOrd, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EvidenceKind {
    InputFact,
    StaticPrediction,
    CatalogEstimate,
    SimulationObservation,
}
