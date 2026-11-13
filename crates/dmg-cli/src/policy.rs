use std::path::Path;

use dmg_core::{
    coverage::{Coverage, CoverageStatus},
    digest::ContentDigest,
    migration::{IntegrityBasis, IntegrityStatus, MigrationDirection, MigrationPlan},
    policy::{DecisionReason, DecisionReasonKind, DecisionStatus, PolicyDecision, PolicyV1},
    risk::{Finding, RuleId, Severity},
};
use thiserror::Error;

use crate::config::{InputError, read_input, toml_location};

const MAX_POLICY_BYTES: u64 = 256 * 1024;

pub(crate) struct ValidatedPolicy {
    policy: PolicyV1,
    digest: ContentDigest,
}

impl ValidatedPolicy {
    pub(crate) fn load(path: &Path) -> Result<Self, PolicyError> {
        let bytes = read_input(path, MAX_POLICY_BYTES)?;
        let input = std::str::from_utf8(&bytes).map_err(|_| PolicyError::Encoding)?;
        let policy = PolicyV1::from_toml(input)
            .map_err(|error| PolicyError::Format(toml_location(input, &error.0)))?;
        if let Some(&id) = policy
            .rules
            .keys()
            .find(|&&id| !dmg_rules::supports_rule(id))
        {
            return Err(PolicyError::UnknownRule(id));
        }
        Ok(Self {
            policy,
            digest: ContentDigest::of(&bytes),
        })
    }

    pub(crate) fn evaluate(
        &self,
        plan: &MigrationPlan,
        coverage: &Coverage,
        findings: &mut [Finding],
    ) -> PolicyDecision {
        self.apply_severity(findings);
        let mut reasons = Vec::new();
        if plan.coverage.status() == CoverageStatus::Incomplete
            || coverage.status() == CoverageStatus::Incomplete
        {
            reasons.push(reason(
                DecisionReasonKind::RequiredCoverage,
                "Required analysis is incomplete",
            ));
        }
        if self.policy.integrity.require_git_base
            && !plan
                .integrity
                .as_ref()
                .is_some_and(|comparison| matches!(comparison.basis, IntegrityBasis::Git { .. }))
        {
            reasons.push(reason(DecisionReasonKind::RequiredEvidence, "The policy requires a complete comparison of the trusted Git base and candidate commits"));
        }
        let status = if reasons.is_empty() {
            self.denials(plan, findings, &mut reasons);
            if reasons.is_empty() {
                DecisionStatus::Pass
            } else {
                DecisionStatus::Denied
            }
        } else {
            DecisionStatus::Incomplete
        };
        reasons.sort();
        reasons.dedup();
        PolicyDecision {
            status,
            policy_digest: self.digest,
            reasons,
        }
    }

    fn apply_severity(&self, findings: &mut [Finding]) {
        for finding in findings {
            if finding.severity != Severity::Critical
                && let Some(policy) = self.policy.rules.get(&finding.rule_id)
            {
                finding.severity = policy.severity;
            }
        }
    }

    fn denials(
        &self,
        plan: &MigrationPlan,
        findings: &[Finding],
        reasons: &mut Vec<DecisionReason>,
    ) {
        for finding in findings {
            if finding.severity >= self.policy.deny_at {
                reasons.push(DecisionReason {
                    kind: DecisionReasonKind::SeverityThreshold,
                    message: format!("{} meets the policy severity threshold", finding.rule_id),
                    finding_fingerprint: Some(finding.fingerprint),
                });
            }
        }
        if let Some(comparison) = &plan.integrity {
            for change in &comparison.changes {
                let status = match change.status {
                    IntegrityStatus::Modified
                        if self.policy.integrity.deny_modified
                            && !change.identity.is_repeatable() =>
                    {
                        "modified"
                    }
                    IntegrityStatus::Missing if self.policy.integrity.deny_missing => "missing",
                    _ => continue,
                };
                let direction = match change.direction {
                    MigrationDirection::Forward => "forward",
                    MigrationDirection::Down => "down",
                };
                let path = change.after.as_ref().or(change.before.as_ref());
                let context = path
                    .map(|file| format!(" ({})", file.path))
                    .unwrap_or_default();
                reasons.push(reason(DecisionReasonKind::IntegrityViolation, format!(
                    "Migration {} {direction} is {status} relative to the integrity baseline{context}",
                    change.identity.version,
                )));
            }
        }
    }
}

fn reason(kind: DecisionReasonKind, message: impl Into<String>) -> DecisionReason {
    DecisionReason {
        kind,
        message: message.into(),
        finding_fingerprint: None,
    }
}

#[derive(Debug, Error)]
pub(crate) enum PolicyError {
    #[error(transparent)]
    Input(#[from] InputError),
    #[error("policy must be UTF-8")]
    Encoding,
    #[error("invalid policy TOML{0}. Check schema_version, field names, and value types")]
    Format(String),
    #[error("rule {0} is unknown in the current ruleset")]
    UnknownRule(RuleId),
}
