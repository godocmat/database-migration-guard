use std::{
    fs,
    path::Path,
    process::{Command, Output},
};

use dmg_core::{
    digest::ContentDigest,
    policy::{PolicyEvidenceV1, PolicySubjectV1, RehearsalEvidencePinV1},
    rehearsal::{RehearsalBindingV1, RehearsalMode, SeedKind},
};
use serde_json::Value;
use tempfile::TempDir;

const PROJECT: &str = r#"schema_version = 1
target = 18
roots = ["migrations"]
search_path = ["public", "app"]

[adapter]
kind = "plain"
filename_pattern = '^([0-9]+)_[a-z]+\.sql$'
version_order = "positive_integer"
transaction = "file"
"#;
const POLICY: &str = r#"schema_version = 1

[evidence]
repository_id = "fixture-repository"
target_id = "fixture-target"

[evidence.rehearsal]
max_age_seconds = 3600
"#;
const REHEARSAL: &str = r#"schema_version = 1
repository_id = "fixture-repository"
target_id = "fixture-target"
schemas = ["public", "app"]
postgres = "18.7"

[setup]
kind = "empty_database"
"#;
const WORKER: &str = r#"
[worker]
docker_executable = "/dmg-cli-fixture-absent/docker"
docker_socket = "/dmg-cli-fixture-absent/docker.sock"
"#;
const BASE_SQL: &str = "CREATE TABLE public.items (id integer);\n";
const HEAD_SQL: &str = "ALTER TABLE public.items ADD COLUMN quantity integer;\n";
const SEED_SQL: &str = "INSERT INTO public.items (id) VALUES (810352);\n";

#[test]
fn offline_binding_is_exact_and_independent_of_worker_and_worktree() {
    let fixture = Fixture::new(HEAD_SQL);
    let output = fixture.run(&["--plan-only"]);
    assert_exit(&output, 0);
    let binding: RehearsalBindingV1 = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(binding.base_commit, fixture.base);
    assert_eq!(binding.head_commit, fixture.head);
    assert_eq!(binding.repository_id.as_str(), "fixture-repository");
    assert_eq!(binding.target_id.as_str(), "fixture-target");
    assert_eq!(binding.scope.schemas(), ["app", "public"]);
    assert_eq!(binding.search_path, ["public", "app"]);
    assert_eq!(binding.server_version_num, 180007);
    assert_eq!(
        binding.project_config_digest,
        ContentDigest::of(PROJECT.as_bytes())
    );
    assert_eq!(binding.policy_digest, ContentDigest::of(POLICY.as_bytes()));
    assert_eq!(binding.mode, RehearsalMode::EmptyDatabase);

    fixture.write(
        "repo/migrations/2_change.sql",
        "a dirty unparseable checkout",
    );
    fixture.rehearsal(&format!("{REHEARSAL}{WORKER}"));
    fixture.write("binding.json", "previous output");
    let saved = fixture.run(&["--plan-only", "--output", "binding.json"]);
    assert_exit(&saved, 0);
    assert!(saved.stdout.is_empty());
    assert_eq!(fixture.read("binding.json"), output.stdout);
}

#[test]
fn setup_binds_exact_baseline_and_seed_without_revealing_source() {
    let fixture = Fixture::new(HEAD_SQL);
    fixture.write("protected/baseline.sql", BASE_SQL);
    fixture.write("protected/seed.sql", SEED_SQL);
    let setup = r#"kind = "schema_only"
baseline_file = "baseline.sql""#;
    fixture.rehearsal(&REHEARSAL.replace("kind = \"empty_database\"", setup));
    let schema_only = fixture.run(&["--plan-only"]);
    assert_exit(&schema_only, 0);
    let schema_only: RehearsalBindingV1 = serde_json::from_slice(&schema_only.stdout).unwrap();
    let RehearsalMode::SchemaOnly { baseline } = schema_only.mode else {
        panic!("expected a schema-only binding");
    };
    assert_eq!(baseline.bytes, BASE_SQL.len() as u64);
    assert_eq!(baseline.digest, ContentDigest::of(BASE_SQL.as_bytes()));

    let setup = r#"kind = "seeded"
baseline_file = "baseline.sql"
seed_file = "seed.sql"
seed_kind = "sanitized""#;
    fixture.rehearsal(&REHEARSAL.replace("kind = \"empty_database\"", setup));
    let output = fixture.run(&["--plan-only"]);
    assert_exit(&output, 0);
    assert!(!String::from_utf8_lossy(&output.stdout).contains("810352"));
    let seeded: RehearsalBindingV1 = serde_json::from_slice(&output.stdout).unwrap();
    let RehearsalMode::Seeded {
        baseline: seeded_baseline,
        seed,
        seed_kind,
    } = seeded.mode
    else {
        panic!("expected a seeded binding");
    };
    assert_eq!(seed_kind, SeedKind::Sanitized);
    assert_eq!(seeded_baseline, baseline);
    assert_eq!(seed.bytes, SEED_SQL.len() as u64);
    assert_eq!(seed.digest, ContentDigest::of(SEED_SQL.as_bytes()));
    assert_eq!(
        seeded.execution_plan_digest,
        schema_only.execution_plan_digest
    );
}

#[test]
fn unsupported_migration_reports_typed_preflight_gaps_before_execution() {
    let fixture = Fixture::new("CREATE SCHEMA private_fixture_schema;\n");
    fixture.rehearsal(&format!("{REHEARSAL}{WORKER}"));
    let output = fixture.run(&[]);
    assert_exit(&output, 3);
    assert!(output.stderr.is_empty());
    let result: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(result["schema_version"], 1);
    assert_eq!(result["outcome"], "incomplete");
    assert!(result.get("binding").is_none());
    let coverage = &result["coverage"];
    assert_eq!(coverage["status"], "incomplete");
    let gap = &coverage["gaps"][0];
    assert_eq!(gap["kind"], "unsupported_sql");
    assert_eq!(gap["scope"]["span"]["path"], "migrations/2_change.sql");
    assert!(!String::from_utf8_lossy(&output.stdout).contains("private_fixture_schema"));
}

#[test]
fn invalid_protected_inputs_leave_an_existing_output_untouched() {
    let fixture = Fixture::new(HEAD_SQL);
    fixture.write("binding.json", "previous output");

    for config in [
        REHEARSAL.replace("schema_version = 1", "schema_version = 2"),
        REHEARSAL.replace("fixture-repository", "wrong-repository"),
        REHEARSAL.replace("fixture-target", "wrong-target"),
        REHEARSAL.replace("18.7", "17.12"),
        REHEARSAL.replace(r#"["public", "app"]"#, r#"["public"]"#),
        REHEARSAL.replace("kind = \"empty_database\"", "kind = \"seeded\""),
        REHEARSAL.replace(
            "kind = \"empty_database\"",
            "kind = \"empty_database\"\nsecret = \"never-print-this-value\"",
        ),
        format!(
            "{REHEARSAL}{}",
            WORKER.replace(
                "/dmg-cli-fixture-absent/docker.sock",
                "tcp://private.invalid:2376"
            )
        ),
        format!("{REHEARSAL}{WORKER}\n[worker.timeouts]\noperation_ms = 300001\n"),
    ] {
        fixture.rehearsal(&config);
        let output = fixture.run(&["--plan-only", "--output", "binding.json"]);
        assert_eq!(output.status.code(), Some(2), "{config}");
        assert!(output.stdout.is_empty());
        assert_eq!(fixture.read("binding.json"), b"previous output");
        assert!(!String::from_utf8_lossy(&output.stderr).contains("never-print-this-value"));
    }

    fixture.rehearsal(REHEARSAL);
    let output = fixture.run(&["--output", "binding.json"]);
    assert_exit(&output, 2);
    assert_eq!(fixture.read("binding.json"), b"previous output");

    let setup = "kind = \"schema_only\"\nbaseline_file = \"baseline.sql\"";
    fixture.rehearsal(&REHEARSAL.replace("kind = \"empty_database\"", setup));
    fixture.write(
        "protected/baseline.sql",
        &"x".repeat(dmg_simulate::MAX_REHEARSAL_SETUP_BYTES + 1),
    );
    let output = fixture.run(&["--plan-only", "--output", "binding.json"]);
    assert_exit(&output, 2);
    assert_eq!(fixture.read("binding.json"), b"previous output");
}

#[test]
fn valid_plan_emits_a_typed_report_when_the_worker_is_unavailable() {
    let fixture = Fixture::new(HEAD_SQL);
    fixture.rehearsal(&format!("{REHEARSAL}{WORKER}"));
    let planned = fixture.run(&["--plan-only"]);
    assert_exit(&planned, 0);
    let expected: RehearsalBindingV1 = serde_json::from_slice(&planned.stdout).unwrap();
    fixture.write("rehearsal.json", "previous output");
    let run = fixture.run(&["--output", "rehearsal.json"]);
    assert_exit(&run, 3);
    assert!(run.stdout.is_empty());
    let bytes = fixture.read("rehearsal.json");
    let report: dmg_core::rehearsal::RehearsalReportV1 = serde_json::from_slice(&bytes).unwrap();
    report.validate().unwrap();
    assert_eq!(report.binding, expected);
    assert_eq!(
        report.outcome,
        dmg_core::rehearsal::RehearsalOutcome::Incomplete
    );
    assert_eq!(
        report.cleanup,
        dmg_core::rehearsal::RehearsalCleanup::NotObserved
    );
    let failure = report.failure.unwrap();
    assert_eq!(
        failure.phase,
        dmg_core::rehearsal::RehearsalPhase::Provision
    );
    #[cfg(not(all(target_os = "linux", target_arch = "x86_64")))]
    assert_eq!(
        failure.reason,
        dmg_core::rehearsal::RehearsalFailureReason::UnsupportedPlatform
    );
    #[cfg(all(target_os = "linux", target_arch = "x86_64"))]
    assert_eq!(
        failure.reason,
        dmg_core::rehearsal::RehearsalFailureReason::Worker
    );
    assert_eq!(report.migrations.len(), 2);
    assert!(!String::from_utf8_lossy(&bytes).contains("/dmg-cli-fixture-absent"));

    let mut bundle = PolicyEvidenceV1::new(PolicySubjectV1 {
        repository_id: expected.repository_id.clone(),
        target_id: expected.target_id.clone(),
        base_commit: expected.base_commit.clone(),
        head_commit: expected.head_commit.clone(),
        policy_digest: expected.policy_digest,
        project_config_digest: expected.project_config_digest,
        ruleset_version: dmg_rules::RULESET_VERSION.into(),
    });
    bundle.rehearsal = Some(RehearsalEvidencePinV1 {
        expected_binding: expected,
        report_sha256: ContentDigest::of(&bytes),
    });
    let bundle = serde_json::to_string(&bundle).unwrap();
    let pin = ContentDigest::of(bundle.as_bytes()).to_string();
    fixture.write("protected/evidence.json", &bundle);
    let checked = fixture
        .command("check")
        .args([
            "--policy-evidence",
            "protected/evidence.json",
            "--policy-evidence-sha256",
            &pin,
            "--rehearsal-report",
            "rehearsal.json",
            "--format",
            "json",
        ])
        .output()
        .unwrap();
    assert_exit(&checked, 3);
    let report: Value = serde_json::from_slice(&checked.stdout).unwrap();
    assert_eq!(report["decision"]["status"], "incomplete");
    let reasons = report["decision"]["reasons"].as_array().unwrap();
    assert!(
        reasons.iter().any(|reason| {
            reason["message"] == "The rehearsal did not complete with the required evidence"
        }),
        "{reasons:?}"
    );
}

struct Fixture {
    dir: TempDir,
    base: String,
    head: String,
}

impl Fixture {
    fn new(candidate: &str) -> Self {
        let dir = tempfile::tempdir().unwrap();
        let repo = dir.path().join("repo");
        fs::create_dir_all(repo.join("migrations")).unwrap();
        fs::create_dir(dir.path().join("protected")).unwrap();
        fs::write(dir.path().join("protected/dmg.toml"), PROJECT).unwrap();
        fs::write(dir.path().join("protected/policy.toml"), POLICY).unwrap();
        git(&repo, &["init", "--quiet"]);
        fs::write(repo.join("migrations/1_create.sql"), BASE_SQL).unwrap();
        git(&repo, &["add", "migrations"]);
        git(&repo, &["commit", "--quiet", "-m", "base"]);
        let base = git(&repo, &["rev-parse", "HEAD"]);
        fs::write(repo.join("migrations/2_change.sql"), candidate).unwrap();
        git(&repo, &["add", "migrations"]);
        git(&repo, &["commit", "--quiet", "-m", "candidate"]);
        let head = git(&repo, &["rev-parse", "HEAD"]);
        let fixture = Self { dir, base, head };
        fixture.rehearsal(REHEARSAL);
        fixture
    }

    fn write(&self, name: &str, contents: &str) {
        fs::write(self.dir.path().join(name), contents).unwrap();
    }

    fn read(&self, name: &str) -> Vec<u8> {
        fs::read(self.dir.path().join(name)).unwrap()
    }

    fn rehearsal(&self, contents: &str) {
        self.write("protected/rehearsal.toml", contents);
    }

    fn run(&self, extra: &[&str]) -> Output {
        self.command("simulate")
            .args(["--rehearsal-config", "protected/rehearsal.toml"])
            .args(extra)
            .output()
            .unwrap()
    }

    fn command(&self, name: &str) -> Command {
        let mut command = Command::new(env!("CARGO_BIN_EXE_dmg"));
        command.current_dir(self.dir.path()).args([
            name,
            "--repo",
            "repo",
            "--base",
            &self.base,
            "--head",
            &self.head,
            "--config",
            "protected/dmg.toml",
            "--policy",
            "protected/policy.toml",
        ]);
        command
    }
}

fn assert_exit(output: &Output, expected: i32) {
    assert_eq!(
        output.status.code(),
        Some(expected),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
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
