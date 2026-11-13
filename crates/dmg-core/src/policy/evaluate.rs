use std::collections::{BTreeMap, BTreeSet};

use crate::{
    coverage::{Coverage, CoverageStatus},
    digest::ContentDigest,
    migration::{
        IntegrityBasis, IntegrityStatus, MigrationDirection, MigrationFile, MigrationPlan,
    },
    rehearsal::RehearsalOutcome,
    risk::{Finding, Severity},
};

use super::{
    ApprovalAuditV1, BaselineMatchV1, BaselineReference, DecisionReason, DecisionReasonKind,
    DecisionStatus, ExpectedRehearsalPlan, FindingApprovalV1, FindingSelectorV1,
    PinnedPolicyEvidence, PolicyAuditV1, PolicyCandidate, PolicyDecision, PolicyEvidenceError,
    PolicyV1, RehearsalAuditV1,
};

pub struct PolicyCheck<'a> {
    pub policy_digest: ContentDigest,
    pub candidate: PolicyCandidate<'a>,
    pub plan: &'a MigrationPlan,
    pub coverage: &'a Coverage,
    pub evidence: Result<Option<&'a PinnedPolicyEvidence>, PolicyEvidenceError>,
    pub baseline_reference: Option<BaselineReference<'a>>,
    pub rehearsal: Result<Option<&'a [u8]>, PolicyEvidenceError>,
    pub expected_rehearsal_plan: Option<&'a ExpectedRehearsalPlan>,
}

pub struct PolicyEvaluation {
    pub decision: PolicyDecision,
    pub audit: Option<PolicyAuditV1>,
}

impl PolicyV1 {
    /// Use the raw policy digest, the discovered candidate, and independently pinned evidence.
    pub fn evaluate(&self, check: PolicyCheck<'_>, findings: &mut [Finding]) -> PolicyEvaluation {
        for finding in findings.iter_mut() {
            if finding.severity != Severity::Critical
                && let Some(policy) = self.rules.get(&finding.rule_id)
            {
                finding.severity = policy.severity;
            }
        }

        let mut incomplete = Vec::new();
        if check.plan.coverage.status() == CoverageStatus::Incomplete
            || check.coverage.status() == CoverageStatus::Incomplete
        {
            incomplete.push(reason(
                DecisionReasonKind::RequiredCoverage,
                "Required analysis is incomplete",
            ));
        }
        if self.integrity.require_git_base
            && !check
                .plan
                .integrity
                .as_ref()
                .is_some_and(|comparison| matches!(comparison.basis, IntegrityBasis::Git { .. }))
        {
            incomplete.push(reason(DecisionReasonKind::RequiredEvidence, "The policy requires a complete comparison of the trusted Git base and candidate commits"));
        }
        if let Err(error) = self.validate_evidence_requirements() {
            incomplete.push(reason(
                DecisionReasonKind::RequiredEvidence,
                error.to_string(),
            ));
        }

        let evidence = record_evidence_error(self.verify_bundle(&check), &mut incomplete).flatten();
        if self
            .evidence
            .as_ref()
            .is_some_and(|policy| policy.require_baseline)
            && !evidence.is_some_and(|bundle| bundle.evidence().baseline.is_some())
        {
            incomplete.push(evidence_reason(PolicyEvidenceError::MissingBaseline));
        }
        let rehearsal = record_evidence_error(
            self.verify_rehearsal(&check, evidence, findings),
            &mut incomplete,
        )
        .flatten();

        let mut audit = evidence.map(|bundle| PolicyAuditV1 {
            evidence_sha256: bundle.digest(),
            repository_id: bundle.evidence().subject.repository_id.clone(),
            target_id: bundle.evidence().subject.target_id.clone(),
            baseline_reference_commit: bundle
                .evidence()
                .baseline
                .as_ref()
                .map(|baseline| baseline.reference_commit.clone()),
            baseline_matches: Vec::new(),
            approvals: Vec::new(),
            rehearsal,
        });
        if incomplete.is_empty()
            && evidence.is_some_and(|bundle| !bundle.evidence().approvals.is_empty())
            && check.candidate.now_unix_seconds.is_none()
        {
            incomplete.push(evidence_reason(PolicyEvidenceError::ClockUnavailable));
        }
        let (status, mut reasons) = if incomplete.is_empty() {
            let mut denials = Vec::new();
            if audit
                .as_ref()
                .and_then(|audit| audit.rehearsal.as_ref())
                .is_some_and(|rehearsal| rehearsal.outcome == RehearsalOutcome::Failed)
            {
                denials.push(reason(
                    DecisionReasonKind::RehearsalFailed,
                    "The verified rehearsal reported a PostgreSQL migration failure",
                ));
            }
            self.finding_denials(&check, findings, evidence.zip(audit.as_mut()), &mut denials);
            self.integrity_denials(check.plan, &mut denials);
            let status = if denials.is_empty() {
                DecisionStatus::Pass
            } else {
                DecisionStatus::Denied
            };
            (status, denials)
        } else {
            (DecisionStatus::Incomplete, incomplete)
        };
        reasons.sort();
        reasons.dedup();
        if let Some(audit) = &mut audit {
            audit.baseline_matches.sort_unstable();
            audit
                .approvals
                .sort_by_key(|approval| approval.finding_fingerprint);
        }
        PolicyEvaluation {
            decision: PolicyDecision {
                status,
                policy_digest: check.policy_digest,
                reasons,
            },
            audit,
        }
    }

    fn verify_bundle<'a>(
        &self,
        check: &PolicyCheck<'a>,
    ) -> Result<Option<&'a PinnedPolicyEvidence>, PolicyEvidenceError> {
        let Some(bundle) = check.evidence? else {
            return Ok(None);
        };
        let policy = self
            .evidence
            .as_ref()
            .ok_or(PolicyEvidenceError::MissingEvidenceScope)?;
        bundle.verify_subject(policy, check.policy_digest, &check.candidate, check.plan)?;
        if let Some(baseline) = &bundle.evidence().baseline {
            let reference = check
                .baseline_reference
                .as_ref()
                .ok_or(PolicyEvidenceError::InvalidBaselineReference)?;
            baseline.verify_reference(reference)?;
        }
        Ok(Some(bundle))
    }

    fn verify_rehearsal(
        &self,
        check: &PolicyCheck<'_>,
        evidence: Option<&PinnedPolicyEvidence>,
        findings: &[Finding],
    ) -> Result<Option<RehearsalAuditV1>, PolicyEvidenceError> {
        let requirement = self
            .evidence
            .as_ref()
            .and_then(|policy| policy.rehearsal.as_ref());
        let required = requirement.is_some_and(|requirement| {
            requirement.rules.is_empty()
                || findings
                    .iter()
                    .any(|finding| requirement.rules.contains(&finding.rule_id))
        });
        let report = check.rehearsal?;
        let Some((bundle, pin)) = evidence.and_then(|bundle| {
            bundle
                .evidence()
                .rehearsal
                .as_ref()
                .map(|pin| (bundle, pin))
        }) else {
            return match (report, required) {
                (Some(_), _) => Err(PolicyEvidenceError::UnpinnedRehearsal),
                (None, true) => Err(PolicyEvidenceError::MissingRehearsal),
                (None, false) => Ok(None),
            };
        };
        let requirement = requirement.ok_or(PolicyEvidenceError::UnpinnedRehearsal)?;
        let bytes = report.ok_or(PolicyEvidenceError::MissingRehearsal)?;
        pin.verify(
            bytes,
            requirement,
            &bundle.evidence().subject,
            &check.candidate,
            check.plan,
            check.expected_rehearsal_plan,
        )
        .map(Some)
    }

    fn finding_denials(
        &self,
        check: &PolicyCheck<'_>,
        findings: &[Finding],
        evidence: Option<(&PinnedPolicyEvidence, &mut PolicyAuditV1)>,
        reasons: &mut Vec<DecisionReason>,
    ) {
        let baseline: BTreeSet<_> = evidence
            .as_ref()
            .and_then(|(bundle, _)| bundle.evidence().baseline.as_ref())
            .into_iter()
            .flat_map(|baseline| &baseline.findings)
            .collect();
        let approvals: BTreeMap<_, _> = evidence
            .as_ref()
            .into_iter()
            .flat_map(|(bundle, _)| &bundle.evidence().approvals)
            .map(|approval| (&approval.finding, approval))
            .collect();
        let mut audit = evidence.map(|(_, audit)| audit);
        let selectors = if baseline.is_empty() && approvals.is_empty() {
            BTreeMap::new()
        } else {
            unique_findings(check.plan, findings)
        };

        for (index, finding) in findings.iter().enumerate() {
            let selector = selectors.get(&index);
            let approval_required = self
                .evidence
                .as_ref()
                .is_some_and(|policy| policy.require_approvals.contains(&finding.rule_id));
            let critical = finding.severity == Severity::Critical;
            let critical_approvable = self
                .evidence
                .as_ref()
                .is_some_and(|policy| policy.allow_critical_approvals.contains(&finding.rule_id));
            let approval_needs_rehearsal = critical
                || self
                    .evidence
                    .as_ref()
                    .is_some_and(|policy| policy.requires_rehearsal_for(finding.rule_id));
            let approval = selector
                .and_then(|selector| approvals.get(selector).copied())
                .filter(|approval| {
                    (!critical || critical_approvable)
                        && approval_matches(
                            approval,
                            check.candidate.now_unix_seconds,
                            audit.as_ref().and_then(|audit| audit.rehearsal.as_ref()),
                            approval_needs_rehearsal,
                        )
                });
            if approval_required && approval.is_none() {
                reasons.push(DecisionReason {
                    kind: DecisionReasonKind::ApprovalRequired,
                    message: format!("{} requires a current scoped approval", finding.rule_id),
                    finding_fingerprint: Some(finding.fingerprint),
                });
            }

            let blocks = finding.severity >= self.deny_at;
            if let Some(approval) = approval.filter(|_| blocks || approval_required) {
                if let Some(audit) = audit.as_deref_mut() {
                    audit.approvals.push(ApprovalAuditV1 {
                        finding_fingerprint: finding.fingerprint,
                        rule_id: finding.rule_id,
                        approver: approval.approver.clone(),
                        justification_sha256: ContentDigest::of(approval.justification.as_bytes()),
                        issued_at_unix_seconds: approval.issued_at_unix_seconds,
                        expires_at_unix_seconds: approval.expires_at_unix_seconds,
                        rehearsal_report_sha256: approval.rehearsal_report_sha256,
                    });
                }
                continue;
            }
            if blocks
                && !critical
                && !approval_required
                && selector.is_some_and(|selector| baseline.contains(selector))
            {
                if let Some(audit) = audit.as_deref_mut() {
                    audit.baseline_matches.push(BaselineMatchV1 {
                        finding_fingerprint: finding.fingerprint,
                        rule_id: finding.rule_id,
                    });
                }
                continue;
            }
            if blocks {
                reasons.push(DecisionReason {
                    kind: DecisionReasonKind::SeverityThreshold,
                    message: format!("{} meets the policy severity threshold", finding.rule_id),
                    finding_fingerprint: Some(finding.fingerprint),
                });
            }
        }
    }

    fn integrity_denials(&self, plan: &MigrationPlan, reasons: &mut Vec<DecisionReason>) {
        let Some(comparison) = &plan.integrity else {
            return;
        };
        for change in &comparison.changes {
            let status = match change.status {
                IntegrityStatus::Modified
                    if self.integrity.deny_modified && !change.identity.is_repeatable() =>
                {
                    "modified"
                }
                IntegrityStatus::Missing if self.integrity.deny_missing => "missing",
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

fn approval_matches(
    approval: &FindingApprovalV1,
    now: Option<u64>,
    rehearsal: Option<&RehearsalAuditV1>,
    require_rehearsal_pin: bool,
) -> bool {
    now.is_some_and(|now| {
        (approval.issued_at_unix_seconds..approval.expires_at_unix_seconds).contains(&now)
    }) && match approval.rehearsal_report_sha256 {
        Some(pin) => rehearsal.is_some_and(|report| {
            report.outcome == RehearsalOutcome::Succeeded
                && report.report_sha256 == pin
                && report.completed_at_unix_seconds <= approval.issued_at_unix_seconds
        }),
        None => !require_rehearsal_pin,
    }
}

pub(super) fn unique_findings(
    plan: &MigrationPlan,
    findings: &[Finding],
) -> BTreeMap<usize, FindingSelectorV1> {
    let mut files = BTreeMap::<_, Option<&MigrationFile>>::new();
    let mut identities = BTreeMap::new();
    for file in plan
        .inventory
        .iter()
        .filter(|file| file.direction == MigrationDirection::Forward)
    {
        *identities.entry(&file.identity).or_insert(0) += 1;
        files
            .entry(&file.path)
            .and_modify(|entry| *entry = None)
            .or_insert(Some(file));
    }
    let mut candidates = BTreeMap::<_, Option<usize>>::new();
    for (index, finding) in findings.iter().enumerate() {
        if let Some(file) = files.get(&finding.span.path).copied().flatten()
            && identities.get(&finding.migration) == Some(&1)
            && let Some(selector) = FindingSelectorV1::for_finding(finding, file)
        {
            candidates
                .entry(selector)
                .and_modify(|entry| *entry = None)
                .or_insert(Some(index));
        }
    }
    candidates
        .into_iter()
        .filter_map(|(selector, index)| index.map(|index| (index, selector)))
        .collect()
}

fn record_evidence_error<T>(
    result: Result<T, PolicyEvidenceError>,
    reasons: &mut Vec<DecisionReason>,
) -> Option<T> {
    match result {
        Ok(value) => Some(value),
        Err(error) => {
            reasons.push(evidence_reason(error));
            None
        }
    }
}

fn evidence_reason(error: PolicyEvidenceError) -> DecisionReason {
    reason(DecisionReasonKind::RequiredEvidence, error.to_string())
}

fn reason(kind: DecisionReasonKind, message: impl Into<String>) -> DecisionReason {
    DecisionReason {
        kind,
        message: message.into(),
        finding_fingerprint: None,
    }
}
