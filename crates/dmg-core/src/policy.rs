//! Versioned policy configuration and decision types.

mod evaluate;
mod evidence;

pub use evaluate::{PolicyCheck, PolicyEvaluation};
pub use evidence::{
    ApprovalAuditV1, BaselineMatchV1, BaselineReference, ExpectedRehearsalPlan, FindingApprovalV1,
    FindingBaselineV1, FindingSelectorV1, MAX_POLICY_EVIDENCE_BYTES, MAX_POLICY_REHEARSAL_BYTES,
    PinnedPolicyEvidence, PolicyAuditV1, PolicyCandidate, PolicyEvidenceError, PolicyEvidenceV1,
    PolicySubjectV1, RehearsalAuditV1, RehearsalEvidencePinV1,
};

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::{
    digest::ContentDigest,
    inspection::TargetId,
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
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub evidence: Option<EvidencePolicy>,
}

impl Default for PolicyV1 {
    fn default() -> Self {
        Self {
            schema_version: SchemaVersionV1,
            deny_at: default_deny_at(),
            integrity: IntegrityPolicy::default(),
            rules: BTreeMap::new(),
            evidence: None,
        }
    }
}

impl PolicyV1 {
    /// Reads the version and the TOML shape. Registry and decision checks use the evaluated ruleset.
    pub fn from_toml(input: &str) -> Result<Self, PolicyFormatError> {
        Ok(toml::from_str(input)?)
    }

    pub fn validate_evidence_requirements(&self) -> Result<(), PolicyRequirementsError> {
        let Some(evidence) = &self.evidence else {
            return Ok(());
        };
        let unique = |rules: &[RuleId]| rules.iter().collect::<BTreeSet<_>>().len() == rules.len();
        if !unique(&evidence.require_approvals)
            || !unique(&evidence.allow_critical_approvals)
            || evidence
                .rehearsal
                .as_ref()
                .is_some_and(|rehearsal| !unique(&rehearsal.rules))
        {
            return Err(PolicyRequirementsError::DuplicateRule);
        }
        if evidence
            .rehearsal
            .as_ref()
            .is_some_and(|rehearsal| rehearsal.max_age_seconds == 0)
        {
            return Err(PolicyRequirementsError::RehearsalAge);
        }
        if evidence.allow_critical_approvals.iter().any(|rule| {
            !evidence.require_approvals.contains(rule) || !evidence.requires_rehearsal_for(*rule)
        }) {
            return Err(PolicyRequirementsError::CriticalApproval);
        }
        Ok(())
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

#[derive(Debug, Clone, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EvidencePolicy {
    pub repository_id: TargetId,
    pub target_id: TargetId,
    #[serde(default)]
    pub require_baseline: bool,
    #[serde(default)]
    pub require_approvals: Vec<RuleId>,
    #[serde(default)]
    pub allow_critical_approvals: Vec<RuleId>,
    pub rehearsal: Option<RehearsalRequirement>,
}

impl EvidencePolicy {
    fn requires_rehearsal_for(&self, rule: RuleId) -> bool {
        self.rehearsal.as_ref().is_some_and(|requirement| {
            requirement.rules.is_empty() || requirement.rules.contains(&rule)
        })
    }

    pub fn rule_ids(&self) -> impl Iterator<Item = RuleId> + '_ {
        self.require_approvals
            .iter()
            .chain(&self.allow_critical_approvals)
            .chain(
                self.rehearsal
                    .iter()
                    .flat_map(|requirement| &requirement.rules),
            )
            .copied()
    }
}

#[derive(Debug, Clone, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RehearsalRequirement {
    /// An empty list requires rehearsal for every check.
    #[serde(default)]
    pub rules: Vec<RuleId>,
    pub max_age_seconds: u64,
    #[serde(default)]
    pub require_complete_schema: bool,
}

#[derive(Debug, Error)]
pub enum PolicyRequirementsError {
    #[error("evidence rule lists must contain unique rule IDs")]
    DuplicateRule,
    #[error("the maximum rehearsal age must be positive")]
    RehearsalAge,
    #[error(
        "each rule permitting a critical approval must also require approval and fresh rehearsal"
    )]
    CriticalApproval,
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
    ApprovalRequired,
    RehearsalFailed,
}
