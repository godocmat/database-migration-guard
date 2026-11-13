use std::{
    collections::BTreeSet,
    path::{Path, PathBuf},
    time::{SystemTime, UNIX_EPOCH},
};

use clap::Args;
use dmg_core::{
    coverage::{Coverage, CoverageStatus},
    digest::ContentDigest,
    inspection::TargetId,
    migration::MigrationPlan,
    policy::{
        BaselineReference, ExpectedRehearsalPlan, FindingBaselineV1, MAX_POLICY_EVIDENCE_BYTES,
        MAX_POLICY_REHEARSAL_BYTES, PinnedPolicyEvidence, PolicyCandidate, PolicyCheck,
        PolicyEvaluation, PolicyEvidenceError, PolicyRequirementsError, PolicyV1,
    },
    risk::{Finding, RuleId},
};
use dmg_discovery::{Discovery, GitRevisions, discover_git};
use dmg_postgres::parse_plan;
use dmg_rules::{Analysis, analyze};
use dmg_simulate::expected_rehearsal_plan;
use thiserror::Error;

use crate::{
    config::{InputError, Project, read_input, toml_location},
    parse_digest,
};

const MAX_POLICY_BYTES: u64 = 256 * 1024;

#[derive(Args)]
pub(crate) struct PolicyEvidenceArgs {
    /// Protected versioned JSON containing a baseline, approvals, or a rehearsal evidence pin.
    #[arg(long, value_name = "FILE", requires = "policy_evidence_sha256")]
    policy_evidence: Option<PathBuf>,

    /// Raw SHA-256 of the policy evidence bundle from a protected input.
    #[arg(long, value_name = "SHA256", requires = "policy_evidence", value_parser = parse_digest)]
    policy_evidence_sha256: Option<ContentDigest>,

    /// Rehearsal report JSON whose exact SHA-256 and binding are pinned in the protected bundle.
    #[arg(long, value_name = "FILE")]
    rehearsal_report: Option<PathBuf>,
}

impl PolicyEvidenceArgs {
    fn load_bundle(&self) -> Result<Option<PinnedPolicyEvidence>, PolicyEvidenceError> {
        self.policy_evidence
            .as_deref()
            .zip(self.policy_evidence_sha256)
            .map(|(path, digest)| {
                let bytes = read_input(path, MAX_POLICY_EVIDENCE_BYTES as u64)
                    .map_err(|_| PolicyEvidenceError::UnreadableBundle)?;
                let bundle = PinnedPolicyEvidence::from_bytes(&bytes, digest)?;
                let records = bundle.evidence();
                if records
                    .baseline
                    .iter()
                    .flat_map(|baseline| &baseline.findings)
                    .chain(records.approvals.iter().map(|approval| &approval.finding))
                    .any(|selector| !dmg_rules::supports_rule(selector.rule_id))
                {
                    return Err(PolicyEvidenceError::UnknownEvidenceRule);
                }
                Ok(bundle)
            })
            .transpose()
    }

    fn load_rehearsal(&self) -> Result<Option<Vec<u8>>, PolicyEvidenceError> {
        self.rehearsal_report
            .as_deref()
            .map(|path| {
                read_input(path, MAX_POLICY_REHEARSAL_BYTES as u64)
                    .map_err(|_| PolicyEvidenceError::UnreadableRehearsal)
            })
            .transpose()
    }
}

pub(crate) struct ValidatedPolicy {
    policy: PolicyV1,
    digest: ContentDigest,
}

pub(crate) struct PolicyAnalysis<'a> {
    pub(crate) repository: &'a Path,
    pub(crate) discovery: &'a Discovery,
    pub(crate) project: &'a Project,
    pub(crate) coverage: &'a Coverage,
}

impl ValidatedPolicy {
    pub(crate) fn load(path: &Path) -> Result<Self, PolicyError> {
        let bytes = read_input(path, MAX_POLICY_BYTES)?;
        let input = std::str::from_utf8(&bytes).map_err(|_| PolicyError::Encoding)?;
        let policy = PolicyV1::from_toml(input)
            .map_err(|error| PolicyError::Format(toml_location(input, &error.0)))?;
        policy.validate_evidence_requirements()?;
        if let Some(id) = policy
            .rules
            .keys()
            .copied()
            .chain(
                policy
                    .evidence
                    .iter()
                    .flat_map(|evidence| evidence.rule_ids()),
            )
            .find(|&id| !dmg_rules::supports_rule(id))
        {
            return Err(PolicyError::UnknownRule(id));
        }
        Ok(Self {
            policy,
            digest: ContentDigest::of(&bytes),
        })
    }

    pub(crate) fn digest(&self) -> ContentDigest {
        self.digest
    }

    pub(crate) fn target_id(&self) -> Option<&TargetId> {
        self.policy.evidence.as_ref().map(|scope| &scope.target_id)
    }

    pub(crate) fn evaluate(
        &self,
        evidence_args: &PolicyEvidenceArgs,
        analysis: PolicyAnalysis<'_>,
        findings: &mut [Finding],
    ) -> PolicyEvaluation {
        let PolicyAnalysis {
            repository,
            discovery,
            project,
            coverage,
        } = analysis;
        let candidate = PolicyCandidate {
            project_config_digest: project.digest,
            ruleset_version: dmg_rules::RULESET_VERSION,
            adapter: discovery.adapter(),
            target: project.target,
            search_path: &project.rules.search_path,
            now_unix_seconds: SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .ok()
                .map(|elapsed| elapsed.as_secs()),
        };
        let bundle = evidence_args.load_bundle().and_then(|bundle| {
            if let Some(bundle) = &bundle {
                let policy = self
                    .policy
                    .evidence
                    .as_ref()
                    .ok_or(PolicyEvidenceError::MissingEvidenceScope)?;
                bundle.verify_subject(policy, self.digest(), &candidate, &discovery.plan)?;
            }
            Ok(bundle)
        });
        let baseline = bundle
            .as_ref()
            .ok()
            .and_then(Option::as_ref)
            .and_then(|bundle| bundle.evidence().baseline.as_ref())
            .and_then(|baseline| read_baseline_reference(repository, project, baseline));
        let report = evidence_args.load_rehearsal();
        let expected = if let (Ok(Some(bundle)), Ok(Some(_))) = (&bundle, &report) {
            bundle.evidence().rehearsal.as_ref().and_then(|pin| {
                let binding = &pin.expected_binding;
                expected_rehearsal_plan(discovery, project.target, &binding.scope, &binding.mode)
                    .ok()
                    .map(|(digest, migrations)| ExpectedRehearsalPlan { digest, migrations })
            })
        } else {
            None
        };

        self.policy.evaluate(
            PolicyCheck {
                policy_digest: self.digest(),
                candidate,
                plan: &discovery.plan,
                coverage,
                evidence: bundle.as_ref().map(Option::as_ref).map_err(|error| *error),
                baseline_reference: baseline.as_ref().map(|(plan, analysis)| BaselineReference {
                    plan,
                    coverage: &analysis.coverage,
                    findings: &analysis.findings,
                }),
                rehearsal: report
                    .as_ref()
                    .map(Option::as_deref)
                    .map_err(|error| *error),
                expected_rehearsal_plan: expected.as_ref(),
            },
            findings,
        )
    }
}

fn read_baseline_reference(
    repository: &Path,
    project: &Project,
    baseline: &FindingBaselineV1,
) -> Option<(MigrationPlan, Analysis)> {
    let reference = &baseline.reference_commit;
    let revisions = GitRevisions::new(reference, reference).ok()?;
    let reference = discover_git(repository, &revisions, &project.discovery);
    if reference.plan.coverage.status() == CoverageStatus::Incomplete {
        return None;
    }
    if baseline.findings.is_empty() {
        return Some((
            reference.plan,
            Analysis {
                coverage: Coverage::Complete,
                findings: Vec::new(),
                locks: Vec::new(),
            },
        ));
    }
    let mut parsed = parse_plan(&reference.plan, project.target, |path| {
        reference.source_bytes(path)
    });
    parsed.selected_forwards = Some(
        baseline
            .findings
            .iter()
            .map(|selector| selector.path.clone())
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect(),
    );
    let analysis = analyze(&parsed, project.target, &project.rules);
    Some((reference.plan, analysis))
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
    #[error(transparent)]
    Requirements(#[from] PolicyRequirementsError),
}
