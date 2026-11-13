use std::collections::BTreeSet;

use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::{
    coverage::{Coverage, CoverageStatus},
    digest::ContentDigest,
    history::forward_inventory_digest,
    inspection::{TargetId, is_catalog_identifier},
    migration::{
        IntegrityBasis, MigrationAdapter, MigrationDirection, MigrationFile, MigrationIdentity,
        MigrationPlan,
    },
    rehearsal::{
        RehearsalBindingV1, RehearsalMigrationState, RehearsalMigrationV1, RehearsalOutcome,
        RehearsalReportV1, RehearsalStatementState,
    },
    risk::{DatabaseObject, Finding, RuleId},
    schema::SchemaVersionV1,
    source::{ByteRange, RepoPath},
    target::PostgresTarget,
};

use super::{EvidencePolicy, RehearsalRequirement};

pub const MAX_POLICY_EVIDENCE_BYTES: usize = 8 * 1024 * 1024;
pub const MAX_POLICY_REHEARSAL_BYTES: usize = 64 * 1024 * 1024;
const MAX_RECORDS: usize = 10_000;

/// Exact scope of the evidence authorized by the protected caller.
#[derive(Debug, Clone, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PolicySubjectV1 {
    pub repository_id: TargetId,
    pub target_id: TargetId,
    pub base_commit: String,
    pub head_commit: String,
    pub policy_digest: ContentDigest,
    pub project_config_digest: ContentDigest,
    pub ruleset_version: String,
}

/// Identity and original source anchor of a single finding.
#[derive(Debug, Clone, Eq, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FindingSelectorV1 {
    pub rule_id: RuleId,
    pub migration: MigrationIdentity,
    pub path: RepoPath,
    pub migration_digest: ContentDigest,
    pub statement_bytes: ByteRange,
    pub finding_fingerprint: ContentDigest,
    pub object: Option<DatabaseObject>,
}

impl FindingSelectorV1 {
    pub(crate) fn for_finding(finding: &Finding, file: &MigrationFile) -> Option<Self> {
        (file.direction == MigrationDirection::Forward
            && finding.migration == file.identity
            && finding.span.path == file.path)
            .then(|| Self {
                rule_id: finding.rule_id,
                migration: finding.migration.clone(),
                path: finding.span.path.clone(),
                migration_digest: file.digest,
                statement_bytes: finding.span.bytes,
                finding_fingerprint: finding.fingerprint,
                object: finding.object.clone(),
            })
    }

    fn is_valid(&self) -> bool {
        !self.statement_bytes.is_empty()
            && self.path.as_str().len() <= 1024
            && !self.path.as_str().chars().any(char::is_control)
            && self.object.as_ref().is_none_or(|object| {
                is_catalog_identifier(&object.name)
                    && object.schema.as_deref().is_none_or(is_catalog_identifier)
                    && object.member.as_deref().is_none_or(is_catalog_identifier)
            })
    }
}

/// Previously reviewed findings recorded at a protected reference commit.
#[derive(Debug, Clone, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FindingBaselineV1 {
    pub reference_commit: String,
    pub findings: Vec<FindingSelectorV1>,
}

/// An independently analyzed Git reference used to verify baseline findings.
pub struct BaselineReference<'a> {
    pub plan: &'a MigrationPlan,
    pub coverage: &'a Coverage,
    pub findings: &'a [Finding],
}

impl FindingBaselineV1 {
    pub(super) fn verify_reference(
        &self,
        reference: &BaselineReference<'_>,
    ) -> Result<(), PolicyEvidenceError> {
        let invalid = PolicyEvidenceError::InvalidBaselineReference;
        if reference.plan.coverage.status() == CoverageStatus::Incomplete
            || reference.coverage.status() == CoverageStatus::Incomplete
            || !matches!(
                reference.plan.integrity.as_ref().map(|comparison| &comparison.basis),
                Some(IntegrityBasis::Git { base_commit, head_commit })
                    if base_commit == &self.reference_commit && head_commit == &self.reference_commit
            )
        {
            return Err(invalid);
        }
        if self.findings.is_empty() {
            return Ok(());
        }
        let matches: BTreeSet<_> =
            super::evaluate::unique_findings(reference.plan, reference.findings)
                .into_values()
                .collect();
        if self
            .findings
            .iter()
            .any(|selector| !matches.contains(selector))
        {
            return Err(invalid);
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FindingApprovalV1 {
    pub finding: FindingSelectorV1,
    pub approver: TargetId,
    pub justification: String,
    pub issued_at_unix_seconds: u64,
    pub expires_at_unix_seconds: u64,
    pub rehearsal_report_sha256: Option<ContentDigest>,
}

impl FindingApprovalV1 {
    fn is_valid(&self) -> bool {
        self.finding.is_valid()
            && valid_text(&self.justification, 1024)
            && self.issued_at_unix_seconds < self.expires_at_unix_seconds
    }
}

/// Expected execution environment and exact report bytes, both authorized by the caller.
#[derive(Debug, Clone, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RehearsalEvidencePinV1 {
    pub expected_binding: RehearsalBindingV1,
    pub report_sha256: ContentDigest,
}

#[derive(Debug, Clone, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PolicyEvidenceV1 {
    schema_version: SchemaVersionV1,
    pub subject: PolicySubjectV1,
    pub baseline: Option<FindingBaselineV1>,
    #[serde(default)]
    pub approvals: Vec<FindingApprovalV1>,
    pub rehearsal: Option<RehearsalEvidencePinV1>,
}

impl PolicyEvidenceV1 {
    pub fn new(subject: PolicySubjectV1) -> Self {
        Self {
            schema_version: SchemaVersionV1,
            subject,
            baseline: None,
            approvals: Vec::new(),
            rehearsal: None,
        }
    }

    fn is_valid(&self) -> bool {
        let subject = &self.subject;
        valid_commit(&subject.base_commit)
            && valid_commit(&subject.head_commit)
            && subject.base_commit.len() == subject.head_commit.len()
            && valid_text(&subject.ruleset_version, 128)
            && self.baseline.as_ref().is_none_or(|baseline| {
                valid_commit(&baseline.reference_commit)
                    && baseline.reference_commit.len() == subject.head_commit.len()
                    && baseline.reference_commit != subject.head_commit
                    && baseline.findings.len() <= MAX_RECORDS
                    && baseline.findings.iter().all(FindingSelectorV1::is_valid)
                    && unique(baseline.findings.iter())
            })
            && self.approvals.len() <= MAX_RECORDS
            && self.approvals.iter().all(FindingApprovalV1::is_valid)
            && unique(self.approvals.iter().map(|approval| &approval.finding))
    }
}

fn valid_commit(commit: &str) -> bool {
    matches!(commit.len(), 40 | 64)
        && commit
            .bytes()
            .all(|byte| matches!(byte, b'0'..=b'9' | b'a'..=b'f'))
}

fn valid_text(text: &str, max_bytes: usize) -> bool {
    !text.trim().is_empty() && text.len() <= max_bytes && !text.chars().any(char::is_control)
}

fn unique<'a>(mut selectors: impl Iterator<Item = &'a FindingSelectorV1>) -> bool {
    let mut seen = BTreeSet::new();
    selectors.all(|selector| seen.insert(selector))
}

/// Evidence authenticated by an expected raw-byte SHA-256 from a protected input.
#[derive(Debug)]
pub struct PinnedPolicyEvidence {
    digest: ContentDigest,
    evidence: PolicyEvidenceV1,
}

impl PinnedPolicyEvidence {
    pub fn from_bytes(
        bytes: &[u8],
        expected_digest: ContentDigest,
    ) -> Result<Self, PolicyEvidenceError> {
        if bytes.len() > MAX_POLICY_EVIDENCE_BYTES {
            return Err(PolicyEvidenceError::UnreadableBundle);
        }
        if ContentDigest::of(bytes) != expected_digest {
            return Err(PolicyEvidenceError::BundleDigest);
        }
        let evidence: PolicyEvidenceV1 =
            serde_json::from_slice(bytes).map_err(|_| PolicyEvidenceError::InvalidBundle)?;
        if !evidence.is_valid() {
            return Err(PolicyEvidenceError::InvalidBundle);
        }
        Ok(Self {
            digest: expected_digest,
            evidence,
        })
    }

    pub fn evidence(&self) -> &PolicyEvidenceV1 {
        &self.evidence
    }

    pub fn digest(&self) -> ContentDigest {
        self.digest
    }

    pub fn verify_subject(
        &self,
        policy: &EvidencePolicy,
        policy_digest: ContentDigest,
        candidate: &PolicyCandidate<'_>,
        plan: &MigrationPlan,
    ) -> Result<(), PolicyEvidenceError> {
        let subject = &self.evidence.subject;
        if subject.repository_id != policy.repository_id
            || subject.target_id != policy.target_id
            || subject.policy_digest != policy_digest
            || subject.project_config_digest != candidate.project_config_digest
            || subject.ruleset_version != candidate.ruleset_version
            || !matches!(
                plan.integrity.as_ref().map(|comparison| &comparison.basis),
                Some(IntegrityBasis::Git { base_commit, head_commit })
                    if base_commit == &subject.base_commit && head_commit == &subject.head_commit
            )
            || plan.selected_forwards.is_none()
        {
            return Err(PolicyEvidenceError::SubjectMismatch);
        }
        Ok(())
    }
}

/// Inputs independently obtained from the configuration and the current evaluation.
#[derive(Debug)]
pub struct PolicyCandidate<'a> {
    pub project_config_digest: ContentDigest,
    pub ruleset_version: &'a str,
    pub adapter: MigrationAdapter,
    pub target: PostgresTarget,
    pub search_path: &'a [String],
    pub now_unix_seconds: Option<u64>,
}

/// The plan reconstructed from Git migration sources and the protected rehearsal scope and mode.
#[derive(Debug)]
pub struct ExpectedRehearsalPlan {
    pub digest: ContentDigest,
    pub migrations: Vec<RehearsalMigrationV1>,
}

#[derive(Debug, Clone, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PolicyAuditV1 {
    pub evidence_sha256: ContentDigest,
    pub repository_id: TargetId,
    pub target_id: TargetId,
    pub baseline_reference_commit: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub baseline_matches: Vec<BaselineMatchV1>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub approvals: Vec<ApprovalAuditV1>,
    pub rehearsal: Option<RehearsalAuditV1>,
}

#[derive(Debug, Clone, Eq, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BaselineMatchV1 {
    pub finding_fingerprint: ContentDigest,
    pub rule_id: RuleId,
}

#[derive(Debug, Clone, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ApprovalAuditV1 {
    pub finding_fingerprint: ContentDigest,
    pub rule_id: RuleId,
    pub approver: TargetId,
    pub justification_sha256: ContentDigest,
    pub issued_at_unix_seconds: u64,
    pub expires_at_unix_seconds: u64,
    pub rehearsal_report_sha256: Option<ContentDigest>,
}

#[derive(Debug, Clone, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RehearsalAuditV1 {
    pub report_sha256: ContentDigest,
    pub binding_sha256: ContentDigest,
    pub completed_at_unix_seconds: u64,
    pub outcome: RehearsalOutcome,
}

impl RehearsalEvidencePinV1 {
    pub(super) fn verify(
        &self,
        bytes: &[u8],
        requirement: &RehearsalRequirement,
        subject: &PolicySubjectV1,
        candidate: &PolicyCandidate<'_>,
        plan: &MigrationPlan,
        expected: Option<&ExpectedRehearsalPlan>,
    ) -> Result<RehearsalAuditV1, PolicyEvidenceError> {
        if bytes.len() > MAX_POLICY_REHEARSAL_BYTES {
            return Err(PolicyEvidenceError::UnreadableRehearsal);
        }
        if ContentDigest::of(bytes) != self.report_sha256 {
            return Err(PolicyEvidenceError::RehearsalDigest);
        }
        let report: RehearsalReportV1 =
            serde_json::from_slice(bytes).map_err(|_| PolicyEvidenceError::InvalidRehearsal)?;
        report
            .validate()
            .map_err(|_| PolicyEvidenceError::InvalidRehearsal)?;
        let binding = &self.expected_binding;
        let expected = expected.ok_or(PolicyEvidenceError::UnresolvedExecution)?;
        let inventory = forward_inventory_digest(candidate.adapter, &plan.inventory)
            .map_err(|_| PolicyEvidenceError::RehearsalBinding)?;
        if binding != &report.binding
            || binding.repository_id != subject.repository_id
            || binding.target_id != subject.target_id
            || binding.base_commit != subject.base_commit
            || binding.head_commit != subject.head_commit
            || binding.policy_digest != subject.policy_digest
            || binding.project_config_digest != candidate.project_config_digest
            || binding.forward_inventory_digest != inventory
            || binding.execution_plan_digest != expected.digest
            || binding.adapter != candidate.adapter
            || binding.target != candidate.target
            || binding.search_path != candidate.search_path
        {
            return Err(PolicyEvidenceError::RehearsalBinding);
        }
        if !matches!(
            report.outcome,
            RehearsalOutcome::Succeeded | RehearsalOutcome::Failed
        ) {
            return Err(PolicyEvidenceError::IncompleteRehearsal);
        }
        if !same_migration_plan(&expected.migrations, &report.migrations)
            || !consistent_outcome(&report)
        {
            return Err(PolicyEvidenceError::InvalidRehearsal);
        }
        let completed = report
            .completed_at_unix_seconds
            .ok_or(PolicyEvidenceError::InvalidRehearsal)?;
        let now = candidate
            .now_unix_seconds
            .ok_or(PolicyEvidenceError::ClockUnavailable)?;
        if now
            .checked_sub(completed)
            .is_none_or(|age| age > requirement.max_age_seconds)
        {
            return Err(PolicyEvidenceError::RehearsalAge);
        }
        if requirement.require_complete_schema
            && report.outcome == RehearsalOutcome::Succeeded
            && !complete_schema_evidence(&report)
        {
            return Err(PolicyEvidenceError::IncompleteRehearsal);
        }
        Ok(RehearsalAuditV1 {
            report_sha256: self.report_sha256,
            binding_sha256: binding
                .digest()
                .map_err(|_| PolicyEvidenceError::InvalidRehearsal)?,
            completed_at_unix_seconds: completed,
            outcome: report.outcome,
        })
    }
}

fn complete_schema_evidence(report: &RehearsalReportV1) -> bool {
    [report.before.as_ref(), report.after.as_ref()]
        .iter()
        .all(|summary| summary.is_some_and(|summary| summary.coverage == CoverageStatus::Complete))
        && report
            .schema_diff
            .as_ref()
            .is_some_and(|diff| diff.coverage == CoverageStatus::Complete)
}

fn same_migration_plan(expected: &[RehearsalMigrationV1], actual: &[RehearsalMigrationV1]) -> bool {
    expected.len() == actual.len()
        && expected.iter().zip(actual).all(|(expected, actual)| {
            expected.identity == actual.identity
                && expected.path == actual.path
                && expected.digest == actual.digest
                && expected.execution == actual.execution
                && expected.groups.len() == actual.groups.len()
                && expected
                    .groups
                    .iter()
                    .zip(&actual.groups)
                    .all(|(expected, actual)| {
                        expected.delivery == actual.delivery
                            && expected
                                .statements
                                .iter()
                                .map(|statement| &statement.span)
                                .eq(actual.statements.iter().map(|statement| &statement.span))
                    })
        })
}

fn consistent_outcome(report: &RehearsalReportV1) -> bool {
    let completed = |migration: &RehearsalMigrationV1| {
        migration.state == RehearsalMigrationState::Completed
            && statements_in_state(migration, RehearsalStatementState::Completed)
    };
    if report.outcome == RehearsalOutcome::Succeeded {
        return report.migrations.iter().all(completed);
    }
    let Some(index) = report
        .migrations
        .iter()
        .position(|migration| migration.state == RehearsalMigrationState::Failed)
    else {
        return false;
    };
    let failed = &report.migrations[index];
    report.migrations[..index].iter().all(completed)
        && report.migrations[index + 1..].iter().all(|migration| {
            migration.state == RehearsalMigrationState::Skipped
                && statements_in_state(migration, RehearsalStatementState::Skipped)
        })
        && report.failure.as_ref().is_some_and(|failure| {
            failure.path.as_ref() == Some(&failed.path)
                && failure.sqlstate.as_deref() != Some("57014")
                && failure.span.as_ref().is_none_or(|span| {
                    failed
                        .groups
                        .iter()
                        .flat_map(|group| &group.statements)
                        .any(|statement| {
                            &statement.span == span
                                && statement.state == RehearsalStatementState::Failed
                        })
                })
        })
}

fn statements_in_state(migration: &RehearsalMigrationV1, state: RehearsalStatementState) -> bool {
    migration
        .groups
        .iter()
        .flat_map(|group| &group.statements)
        .all(|statement| statement.state == state)
}

#[derive(Debug, Clone, Copy, Eq, Error, PartialEq)]
pub enum PolicyEvidenceError {
    #[error("Could not read the bounded policy evidence bundle")]
    UnreadableBundle,
    #[error("The policy evidence bundle differs from its protected SHA-256")]
    BundleDigest,
    #[error("The policy evidence bundle is malformed or contains ambiguous records")]
    InvalidBundle,
    #[error("The policy evidence bundle references a rule outside the current ruleset")]
    UnknownEvidenceRule,
    #[error("The protected policy must identify the repository and target for policy evidence")]
    MissingEvidenceScope,
    #[error(
        "The policy evidence scope differs from the trusted repository, target, Git revision, configuration, policy, or ruleset"
    )]
    SubjectMismatch,
    #[error("The policy requires a trusted finding baseline")]
    MissingBaseline,
    #[error(
        "Could not independently verify the trusted baseline against the reviewed Git reference"
    )]
    InvalidBaselineReference,
    #[error("The policy requires a fresh rehearsal report and a protected evidence pin")]
    MissingRehearsal,
    #[error(
        "A rehearsal report requires a protected binding and report SHA-256, and a policy freshness limit"
    )]
    UnpinnedRehearsal,
    #[error("Could not read the bounded rehearsal report")]
    UnreadableRehearsal,
    #[error("The rehearsal report differs from its protected SHA-256")]
    RehearsalDigest,
    #[error("The rehearsal report is malformed or its migration observations are inconsistent")]
    InvalidRehearsal,
    #[error("Could not independently resolve the exact rehearsal execution plan")]
    UnresolvedExecution,
    #[error(
        "The rehearsal binding differs from the protected binding or the independently checked candidate"
    )]
    RehearsalBinding,
    #[error("The rehearsal did not complete with the required evidence")]
    IncompleteRehearsal,
    #[error("The rehearsal is stale or its completion time is in the future")]
    RehearsalAge,
    #[error("Could not determine the current time to check policy evidence freshness")]
    ClockUnavailable,
}
