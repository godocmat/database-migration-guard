//! Versioned JSON analysis report.

use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::{
    coverage::{Coverage, CoverageGap},
    migration::MigrationPlan,
    policy::{PolicyAuditV1, PolicyDecision},
    risk::{Finding, RelationLockObservation, Severity},
    schema::SchemaVersionV1,
    target::PostgresTarget,
};

pub const REPORT_SCHEMA_VERSION: u32 = SchemaVersionV1::NUMBER;

#[derive(Debug, Clone, Eq, PartialEq, Serialize, Deserialize)]
pub struct ReportV1 {
    schema_version: SchemaVersionV1,
    #[serde(flatten)]
    metadata: ReportMetadata,
    migrations: MigrationPlan,
    coverage: Coverage,
    severity_summary: SeveritySummary,
    findings: Vec<Finding>,
    decision: Option<PolicyDecision>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    policy_audit: Option<PolicyAuditV1>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    relation_locks: Vec<RelationLockObservation>,
}

impl ReportV1 {
    /// Builds a report with sorted findings, evidence, and decision reasons.
    pub fn new(
        metadata: ReportMetadata,
        migrations: MigrationPlan,
        analysis_gaps: impl IntoIterator<Item = CoverageGap>,
        mut findings: Vec<Finding>,
        mut decision: Option<PolicyDecision>,
    ) -> Self {
        let mut gaps = migrations.coverage.gaps().to_vec();
        gaps.extend(analysis_gaps);
        let coverage = Coverage::from_gaps(gaps);
        let mut severity_summary = SeveritySummary::default();
        for finding in &mut findings {
            finding.evidence.sort();
            finding.evidence.dedup();
            severity_summary.record(finding.severity);
        }
        findings.sort();
        if let Some(decision) = &mut decision {
            decision.reasons.sort();
            decision.reasons.dedup();
        }

        Self {
            schema_version: SchemaVersionV1,
            metadata,
            migrations,
            coverage,
            severity_summary,
            findings,
            decision,
            policy_audit: None,
            relation_locks: Vec::new(),
        }
    }

    pub fn with_relation_locks(mut self, mut locks: Vec<RelationLockObservation>) -> Self {
        locks.sort();
        locks.dedup();
        self.relation_locks = locks;
        self
    }

    pub fn with_policy_audit(mut self, audit: Option<PolicyAuditV1>) -> Self {
        self.policy_audit = audit;
        self
    }

    pub fn from_json(input: &str) -> Result<Self, ReportFormatError> {
        Ok(serde_json::from_str(input)?)
    }

    pub fn metadata(&self) -> &ReportMetadata {
        &self.metadata
    }

    pub fn migrations(&self) -> &MigrationPlan {
        &self.migrations
    }

    pub fn coverage(&self) -> &Coverage {
        &self.coverage
    }

    pub fn severity_summary(&self) -> &SeveritySummary {
        &self.severity_summary
    }

    pub fn findings(&self) -> &[Finding] {
        &self.findings
    }

    pub fn decision(&self) -> Option<&PolicyDecision> {
        self.decision.as_ref()
    }

    pub fn policy_audit(&self) -> Option<&PolicyAuditV1> {
        self.policy_audit.as_ref()
    }

    pub fn relation_locks(&self) -> &[RelationLockObservation] {
        &self.relation_locks
    }
}

#[derive(Debug, Clone, Eq, PartialEq, Serialize, Deserialize)]
pub struct ReportMetadata {
    pub tool_version: String,
    pub ruleset_version: String,
    pub target: PostgresTarget,
}

#[derive(Debug, Default, Clone, Eq, PartialEq, Serialize, Deserialize)]
pub struct SeveritySummary {
    pub info: usize,
    pub low: usize,
    pub medium: usize,
    pub high: usize,
    pub critical: usize,
}

impl SeveritySummary {
    fn record(&mut self, severity: Severity) {
        match severity {
            Severity::Info => self.info += 1,
            Severity::Low => self.low += 1,
            Severity::Medium => self.medium += 1,
            Severity::High => self.high += 1,
            Severity::Critical => self.critical += 1,
        }
    }
}

#[derive(Debug, Error)]
#[error("invalid versioned report JSON")]
pub struct ReportFormatError(#[from] pub serde_json::Error);
