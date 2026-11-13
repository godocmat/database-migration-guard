use std::{
    fs,
    path::Path,
    process::{Command, Output},
    time::{SystemTime, UNIX_EPOCH},
};

use dmg_core::{
    coverage::CoverageStatus,
    diff::compare_snapshots,
    digest::ContentDigest,
    inspection::{
        InspectionEvidenceV1, InspectionScope, Observation, SchemaObservation, SchemaState,
        TargetId,
    },
    policy::{
        FindingApprovalV1, FindingBaselineV1, FindingSelectorV1, PolicyEvidenceV1, PolicySubjectV1,
        RehearsalEvidencePinV1,
    },
    rehearsal::{
        RehearsalCleanup, RehearsalDurability, RehearsalFailureReason, RehearsalFailureV1,
        RehearsalMigrationState, RehearsalOutcome, RehearsalPhase, RehearsalReportV1,
        RehearsalSchemaV1, RehearsalStatementState,
    },
    report::ReportV1,
    snapshot::SchemaSnapshotV1,
    source::RepoPath,
    target::PostgresTarget,
};
use dmg_discovery::{
    Adapter, DiscoveryLimits, DiscoveryOptions, GitRevisions, PlainSql, PlainTransaction,
    VersionOrder, discover_git,
};
use dmg_simulate::{PostgresImage, RehearsalContext, RehearsalInput, prepare_rehearsal};
use tempfile::TempDir;

const BASE_SQL: &str = "CREATE TABLE public.items (id integer);\n";
const RISKY_SQL: &str = "UPDATE public.items SET id = 2;\n";
const DESTRUCTIVE_SQL: &str = "DROP TABLE public.items;\n";
const NEW_MIGRATION: &str = "migrations/2_change.sql";
const CONFIG: &str = r#"schema_version = 1
target = 18
roots = ["migrations"]

[adapter]
kind = "plain"
filename_pattern = '^([0-9]+)_[a-z]+\.sql$'
version_order = "positive_integer"
transaction = "file"

[exit_codes]
denied = 41
incomplete = 43
"#;
const POLICY: &str = r#"schema_version = 1

[evidence]
repository_id = "fixture-repository"
target_id = "fixture-target"
"#;

#[test]
fn current_scoped_approval_passes_and_audit_omits_justification() {
    let fixture = Fixture::new(
        RISKY_SQL,
        &format!("{POLICY}require_approvals = ['PG006']\n"),
    );
    let selector = fixture.selector("PG006");
    let now = now();
    let approval = approval(selector.clone(), now);
    let mut bundle = PolicyEvidenceV1::new(fixture.subject());
    assert_eq!(fixture.check(41, &[]).findings().len(), 1);
    bundle.approvals.push(approval.clone());
    let report = fixture.with_bundle(&bundle, None, 0);
    assert_eq!(
        report.findings()[0].fingerprint,
        selector.finding_fingerprint
    );
    let audit = &report.policy_audit().unwrap().approvals[0];
    assert_eq!(audit.approver, approval.approver);
    assert_eq!(
        audit.expires_at_unix_seconds,
        approval.expires_at_unix_seconds
    );
    assert_eq!(
        audit.justification_sha256,
        ContentDigest::of(approval.justification.as_bytes())
    );
    assert!(
        !serde_json::to_string(&report)
            .unwrap()
            .contains(&approval.justification)
    );

    bundle.approvals[0].expires_at_unix_seconds = now;
    assert!(
        fixture
            .with_bundle(&bundle, None, 41)
            .policy_audit()
            .unwrap()
            .approvals
            .is_empty()
    );

    bundle.approvals[0] = approval;
    bundle.approvals[0].finding.migration_digest = ContentDigest::of(b"edited migration");
    fixture.with_bundle(&bundle, None, 41);

    bundle.subject.target_id = TargetId::new("different-target").unwrap();
    assert!(
        fixture
            .with_bundle(&bundle, None, 43)
            .policy_audit()
            .is_none()
    );
    bundle.subject = fixture.subject();
    bundle.subject.head_commit = fixture.base.clone();
    fixture.with_bundle(&bundle, None, 43);
}

#[test]
fn trusted_baselines_match_exact_original_bytes_and_cannot_override_required_approval() {
    let mut fixture = Fixture::new(RISKY_SQL, &format!("{POLICY}require_baseline = true\n"));
    let selector = fixture.selector("PG006");
    let mut bundle = PolicyEvidenceV1::new(fixture.subject());
    fixture.check(43, &[]);
    bundle.baseline = Some(FindingBaselineV1 {
        reference_commit: fixture.reviewed_reference(),
        findings: vec![selector.clone()],
    });
    let report = fixture.with_bundle(&bundle, None, 0);
    assert_eq!(
        report.policy_audit().unwrap().baseline_matches[0].finding_fingerprint,
        selector.finding_fingerprint
    );
    bundle.baseline.as_mut().unwrap().findings[0].path =
        RepoPath::new("migrations/2_moved.sql").unwrap();
    fixture.with_bundle(&bundle, None, 43);

    bundle.baseline.as_mut().unwrap().findings = vec![selector.clone()];
    let real_reference = bundle.baseline.as_ref().unwrap().reference_commit.clone();
    bundle.baseline.as_mut().unwrap().reference_commit = fixture.base.clone();
    fixture.with_bundle(&bundle, None, 43);
    bundle.baseline.as_mut().unwrap().reference_commit = real_reference;

    bundle.baseline.as_mut().unwrap().findings = vec![selector.clone(), selector.clone()];
    assert!(
        fixture
            .with_bundle(&bundle, None, 43)
            .policy_audit()
            .is_none()
    );

    bundle.baseline.as_mut().unwrap().findings = vec![selector];
    fixture.set_policy(&format!("{POLICY}require_approvals = ['PG006']\n"));
    bundle.subject = fixture.subject();
    fixture.with_bundle(&bundle, None, 41);

    fixture.set_policy(&format!("{POLICY}require_baseline = true\n"));
    fixture.commit_change(NEW_MIGRATION, "UPDATE public.items SET id = 3;\n");
    bundle.subject = fixture.subject();
    let report = fixture.with_bundle(&bundle, None, 41);
    assert!(report.policy_audit().unwrap().baseline_matches.is_empty());
}

#[test]
fn forged_bundle_and_independently_required_coverage_are_incomplete() {
    let fixture = Fixture::new(RISKY_SQL, POLICY);
    let mut bundle = PolicyEvidenceV1::new(fixture.subject());
    bundle
        .approvals
        .push(approval(fixture.selector("PG006"), now()));
    let bytes = serde_json::to_vec(&bundle).unwrap();
    let pin = ContentDigest::of(&bytes).to_string();
    fs::write(
        fixture.path().join("evidence.json"),
        [bytes, b"\n".to_vec()].concat(),
    )
    .unwrap();
    fixture.check(
        43,
        &[
            "--policy-evidence",
            "evidence.json",
            "--policy-evidence-sha256",
            &pin,
        ],
    );

    let mut fixture = fixture;
    fixture.commit_change("migrations/3_unsupported.sql", "VACUUM public.items;\n");
    bundle.subject = fixture.subject();
    let report = fixture.with_bundle(&bundle, None, 43);
    assert!(report.policy_audit().unwrap().approvals.is_empty());

    let mut fixture = Fixture::new(RISKY_SQL, POLICY);
    fixture.commit_change(
        "migrations/1_create.sql",
        "CREATE TABLE public.items (id bigint);\n",
    );
    let mut bundle = PolicyEvidenceV1::new(fixture.subject());
    bundle
        .approvals
        .push(approval(fixture.selector("PG006"), now()));
    let report = fixture.with_bundle(&bundle, None, 41);
    assert_eq!(
        report.decision().unwrap().reasons[0].kind,
        dmg_core::policy::DecisionReasonKind::IntegrityViolation
    );
}

#[test]
fn critical_findings_require_explicit_approval_and_the_exact_successful_rehearsal() {
    let rehearsal_policy = r#"
require_approvals = ["PG001"]
allow_critical_approvals = ["PG001"]

[evidence.rehearsal]
rules = ["PG001"]
max_age_seconds = 900
require_complete_schema = true
"#;
    let fixture = Fixture::new(DESTRUCTIVE_SQL, &format!("{POLICY}{rehearsal_policy}"));
    let selector = fixture.selector("PG001");
    let now = now();
    let report = fixture.rehearsal(now - 10);
    let mut bundle = PolicyEvidenceV1::new(fixture.subject());
    bundle.baseline = Some(FindingBaselineV1 {
        reference_commit: fixture.reviewed_reference(),
        findings: vec![selector.clone()],
    });
    fixture.check(43, &[]);
    let mut destructive_approval = approval(selector, now);
    bundle.approvals.push(destructive_approval.clone());
    fixture.with_rehearsal(&mut bundle, &report, 41);

    destructive_approval.rehearsal_report_sha256 =
        Some(ContentDigest::of(&serde_json::to_vec(&report).unwrap()));
    bundle.approvals[0] = destructive_approval;
    let compatibility = fixture.selector("PG002");
    let result = fixture.with_rehearsal(&mut bundle, &report, 41);
    let audit = result.policy_audit().unwrap();
    assert_eq!(audit.approvals.len(), 1);
    assert_eq!(audit.approvals[0].rule_id.to_string(), "PG001");
    assert!(
        result.decision().unwrap().reasons.iter().any(|reason| {
            reason.finding_fingerprint == Some(compatibility.finding_fingerprint)
        })
    );

    bundle.approvals.push(approval(compatibility, now));
    let result = fixture.with_rehearsal(&mut bundle, &report, 0);
    assert_eq!(
        result
            .findings()
            .iter()
            .find(|finding| finding.rule_id.to_string() == "PG001")
            .unwrap()
            .severity,
        dmg_core::risk::Severity::Critical
    );
    let audit = result.policy_audit().unwrap();
    assert!(audit.baseline_matches.is_empty());
    assert_eq!(audit.approvals.len(), 2);
    assert_eq!(
        audit.rehearsal.as_ref().unwrap().binding_sha256,
        report.binding.digest().unwrap()
    );

    bundle.approvals[0].rehearsal_report_sha256 = Some(ContentDigest::of(b"another rehearsal"));
    fixture.with_rehearsal(&mut bundle, &report, 41);
}

#[test]
fn exact_rehearsal_can_deny_and_invalid_observations_are_incomplete() {
    let fixture = Fixture::new(
        RISKY_SQL,
        &format!("{POLICY}[evidence.rehearsal]\nmax_age_seconds = 900\n"),
    );
    let now = now();
    let report = fixture.rehearsal(now - 10);
    let mut bundle = PolicyEvidenceV1::new(fixture.subject());
    // The successful rehearsal leaves the independently blocking static finding in force.
    fixture.with_rehearsal(&mut bundle, &report, 41);
    for completed in [now - 901, now + 3600] {
        fixture.with_rehearsal(&mut bundle, &fixture.rehearsal(completed), 43);
    }

    let mut wrong = report.clone();
    wrong.binding.execution_plan_digest = ContentDigest::of(b"forged execution plan");
    fixture.with_rehearsal(&mut bundle, &wrong, 43);
    wrong = report.clone();
    wrong.migrations.pop();
    fixture.with_rehearsal(&mut bundle, &wrong, 43);
    wrong = report.clone();
    wrong.cleanup = RehearsalCleanup::Unconfirmed;
    fixture.with_rehearsal(&mut bundle, &wrong, 43);

    let mut failed = report;
    let migration = failed.migrations.last_mut().unwrap();
    migration.state = RehearsalMigrationState::Failed;
    let statement = &mut migration.groups[0].statements[0];
    statement.state = RehearsalStatementState::Failed;
    statement.durability = None;
    failed.outcome = RehearsalOutcome::Failed;
    failed.failure = Some(RehearsalFailureV1 {
        phase: RehearsalPhase::Migration,
        reason: RehearsalFailureReason::Database,
        sqlstate: Some("23505".into()),
        path: Some(migration.path.clone()),
        span: Some(statement.span.clone()),
    });
    let report = fixture.with_rehearsal(&mut bundle, &failed, 41);
    assert!(
        report
            .decision()
            .unwrap()
            .reasons
            .iter()
            .any(|reason| reason.kind == dmg_core::policy::DecisionReasonKind::RehearsalFailed)
    );
    assert_eq!(
        report
            .policy_audit()
            .unwrap()
            .rehearsal
            .as_ref()
            .unwrap()
            .outcome,
        RehearsalOutcome::Failed
    );
    let mut changed = serde_json::to_vec(&failed).unwrap();
    changed.push(b'\n');
    fixture.with_bundle(&bundle, Some(&changed), 43);
    failed.migrations.last_mut().unwrap().state = RehearsalMigrationState::Skipped;
    fixture.with_rehearsal(&mut bundle, &failed, 43);
}

fn approval(finding: FindingSelectorV1, now: u64) -> FindingApprovalV1 {
    FindingApprovalV1 {
        finding,
        approver: TargetId::new("reviewer-01").unwrap(),
        justification: "Reviewed in the private change request".into(),
        issued_at_unix_seconds: now - 5,
        expires_at_unix_seconds: now + 300,
        rehearsal_report_sha256: None,
    }
}

fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs()
}

struct Fixture {
    dir: TempDir,
    base: String,
    head: String,
    policy_digest: ContentDigest,
}

impl Fixture {
    fn new(sql: &str, policy: &str) -> Self {
        let dir = tempfile::tempdir().unwrap();
        git(dir.path(), &["init", "--quiet"]);
        fs::create_dir(dir.path().join("migrations")).unwrap();
        fs::write(dir.path().join("dmg.toml"), CONFIG).unwrap();
        fs::write(dir.path().join("migrations/1_create.sql"), BASE_SQL).unwrap();
        git(dir.path(), &["add", "migrations"]);
        git(dir.path(), &["commit", "--quiet", "-m", "base"]);
        let base = git(dir.path(), &["rev-parse", "HEAD"]);
        let mut fixture = Self {
            dir,
            base,
            head: String::new(),
            policy_digest: ContentDigest::of(&[]),
        };
        fixture.set_policy(policy);
        fixture.commit_change(NEW_MIGRATION, sql);
        fixture
    }

    fn path(&self) -> &Path {
        self.dir.path()
    }

    fn set_policy(&mut self, policy: &str) {
        fs::write(self.path().join("policy.toml"), policy).unwrap();
        self.policy_digest = ContentDigest::of(policy.as_bytes());
    }

    fn commit_change(&mut self, path: &str, contents: &str) {
        fs::write(self.path().join(path), contents).unwrap();
        git(self.path(), &["add", path]);
        git(self.path(), &["commit", "--quiet", "-m", "candidate"]);
        self.head = git(self.path(), &["rev-parse", "HEAD"]);
    }

    fn subject(&self) -> PolicySubjectV1 {
        PolicySubjectV1 {
            repository_id: TargetId::new("fixture-repository").unwrap(),
            target_id: TargetId::new("fixture-target").unwrap(),
            base_commit: self.base.clone(),
            head_commit: self.head.clone(),
            policy_digest: self.policy_digest,
            project_config_digest: ContentDigest::of(CONFIG.as_bytes()),
            ruleset_version: dmg_rules::RULESET_VERSION.into(),
        }
    }

    fn reviewed_reference(&self) -> String {
        let tree = git(self.path(), &["rev-parse", "HEAD^{tree}"]);
        git(
            self.path(),
            &[
                "commit-tree",
                &tree,
                "-p",
                &self.base,
                "-m",
                "reviewed reference",
            ],
        )
    }

    fn selector(&self, rule: &str) -> FindingSelectorV1 {
        let report = self.run(&[
            "scan", "--base", &self.base, "--head", &self.head, "--format", "json",
        ]);
        assert_eq!(report.status.code(), Some(0));
        let report = decode(&report);
        let finding = report
            .findings()
            .iter()
            .find(|finding| finding.rule_id.to_string() == rule)
            .unwrap();
        let file = report
            .migrations()
            .inventory
            .iter()
            .find(|file| file.path == finding.span.path)
            .unwrap();
        FindingSelectorV1 {
            rule_id: finding.rule_id,
            migration: finding.migration.clone(),
            path: finding.span.path.clone(),
            migration_digest: file.digest,
            statement_bytes: finding.span.bytes,
            finding_fingerprint: finding.fingerprint,
            object: finding.object.clone(),
        }
    }

    fn check(&self, code: i32, extra: &[&str]) -> ReportV1 {
        let output = self.run(
            &[
                "check",
                "--base",
                &self.base,
                "--head",
                &self.head,
                "--policy",
                "policy.toml",
                "--format",
                "json",
            ]
            .into_iter()
            .chain(extra.iter().copied())
            .collect::<Vec<_>>(),
        );
        assert_eq!(
            output.status.code(),
            Some(code),
            "stdout\n{}\nstderr\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        decode(&output)
    }

    fn with_bundle(&self, bundle: &PolicyEvidenceV1, report: Option<&[u8]>, code: i32) -> ReportV1 {
        let bytes = serde_json::to_vec(bundle).unwrap();
        let digest = ContentDigest::of(&bytes).to_string();
        fs::write(self.path().join("evidence.json"), bytes).unwrap();
        let mut args = vec![
            "--policy-evidence",
            "evidence.json",
            "--policy-evidence-sha256",
            &digest,
        ];
        if let Some(report) = report {
            fs::write(self.path().join("rehearsal.json"), report).unwrap();
            args.extend(["--rehearsal-report", "rehearsal.json"]);
        }
        self.check(code, &args)
    }

    fn rehearsal(&self, completed: u64) -> RehearsalReportV1 {
        let options = DiscoveryOptions::new(
            vec![RepoPath::new("migrations").unwrap()],
            Adapter::Plain(
                PlainSql::new(
                    r"^([0-9]+)_[a-z]+\.sql$",
                    VersionOrder::PositiveInteger,
                    PlainTransaction::File,
                )
                .unwrap(),
            ),
            DiscoveryLimits::default(),
        )
        .unwrap();
        let discovery = discover_git(
            self.path(),
            &GitRevisions::new(&self.base, &self.head).unwrap(),
            &options,
        );
        let subject = self.subject();
        let prepared = prepare_rehearsal(
            &discovery,
            RehearsalContext {
                repository_id: subject.repository_id,
                target: PostgresTarget::V18,
                target_id: subject.target_id,
                scope: InspectionScope::new(vec!["public".into()]).unwrap(),
                search_path: vec!["public".into()],
                project_config_digest: subject.project_config_digest,
                policy_digest: subject.policy_digest,
            },
            RehearsalInput::EmptyDatabase,
            PostgresImage::V18_7,
        )
        .unwrap();
        let binding = prepared.binding();
        let mut report = RehearsalReportV1::new(binding.clone(), completed - 2);
        report.completed_at_unix_seconds = Some(completed);
        report.outcome = RehearsalOutcome::Succeeded;
        report.cleanup = RehearsalCleanup::Verified;
        report.migrations = prepared.expected_migrations();
        for migration in &mut report.migrations {
            migration.state = RehearsalMigrationState::Completed;
            migration.elapsed_millis = Some(1);
            for group in &mut migration.groups {
                group.elapsed_millis = Some(1);
                for statement in &mut group.statements {
                    statement.state = RehearsalStatementState::Completed;
                    statement.durability = Some(RehearsalDurability::Committed);
                }
            }
        }
        let snapshot =
            SchemaSnapshotV1::new(vec!["public".into()], vec![], vec![], vec![], vec![]).unwrap();
        let observation = |time| {
            let summary = RehearsalSchemaV1 {
                structural_digest: snapshot.digest().unwrap(),
                server_version_num: binding.server_version_num,
                observed_at_unix_seconds: time,
                coverage: CoverageStatus::Complete,
                constraints: Vec::new(),
            };
            let evidence = InspectionEvidenceV1::new(
                Observation {
                    target_id: binding.target_id.clone(),
                    server_version_num: Some(binding.server_version_num),
                    observed_at_unix_seconds: time,
                },
                vec![SchemaObservation {
                    schema: "public".into(),
                    state: SchemaState::Visible,
                }],
                Some(snapshot.clone()),
                Vec::new(),
                None,
                Vec::new(),
            )
            .unwrap();
            (summary, evidence)
        };
        let (before, before_evidence) = observation(completed - 2);
        let (after, after_evidence) = observation(completed - 1);
        report.before = Some(before);
        report.after = Some(after);
        report.schema_diff =
            Some(compare_snapshots(&before_evidence, &after_evidence, &[]).unwrap());
        report.validate().unwrap();
        report
    }

    fn with_rehearsal(
        &self,
        bundle: &mut PolicyEvidenceV1,
        report: &RehearsalReportV1,
        code: i32,
    ) -> ReportV1 {
        let bytes = serde_json::to_vec(report).unwrap();
        bundle.rehearsal = Some(RehearsalEvidencePinV1 {
            expected_binding: report.binding.clone(),
            report_sha256: ContentDigest::of(&bytes),
        });
        self.with_bundle(bundle, Some(&bytes), code)
    }

    fn run(&self, args: &[&str]) -> Output {
        Command::new(env!("CARGO_BIN_EXE_dmg"))
            .current_dir(self.path())
            .args(args)
            .output()
            .unwrap()
    }
}

fn decode(output: &Output) -> ReportV1 {
    ReportV1::from_json(std::str::from_utf8(&output.stdout).unwrap()).unwrap()
}

fn git(directory: &Path, args: &[&str]) -> String {
    let output = Command::new("git")
        .current_dir(directory)
        .args([
            "-c",
            "user.name=Migration Fixture",
            "-c",
            "user.email=fixture@example.invalid",
            "-c",
            "commit.gpgSign=false",
            "-c",
            "core.autocrlf=false",
        ])
        .args(args)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).unwrap().trim().to_owned()
}
