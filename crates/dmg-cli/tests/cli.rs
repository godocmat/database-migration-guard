use std::{
    fs,
    path::{Path, PathBuf},
    process::{Command, Output},
};

use dmg_core::digest::ContentDigest;
use serde_json::{Value, json};
use tempfile::TempDir;

const CONFIG: &str = r#"schema_version = 1
target = 18
roots = ["migrations"]

[adapter]
kind = "plain"
filename_pattern = '^([0-9]+)_[a-z][a-z0-9_]*\.sql$'
version_order = "positive_integer"
transaction = "file"
"#;
const POLICY: &str = "schema_version = 1\n";

#[test]
fn renders_a_denied_saved_report_without_replacing_the_gate_status() {
    let project = Fixture::new();
    project.write(
        "migrations/002_update.sql",
        "UPDATE public.items SET id = 7;\n",
    );
    let check = project.run(&[
        "check",
        "--policy",
        "policy.toml",
        "--format",
        "json",
        "--output",
        "report.json",
    ]);
    assert_exit(&check, 1);
    let markdown = project.run(&[
        "report",
        "--input",
        "report.json",
        "--format",
        "markdown",
        "--output",
        "summary.md",
    ]);
    assert_exit(&markdown, 0);
    assert!(markdown.stdout.is_empty());
    let summary = fs::read_to_string(project.path("summary.md")).unwrap();
    assert!(summary.contains("PG006"));
    let annotations = project.run(&["report", "--input", "report.json", "--format", "github"]);
    assert_exit(&annotations, 0);
    let annotations = String::from_utf8(annotations.stdout).unwrap();
    assert!(annotations.contains("::error "));
    assert!(annotations.contains("migrations/002_update.sql"));
    assert_eq!(project.read_report()["decision"]["status"], "denied");

    let html = project.run(&["report", "--input", "report.json", "--format", "html"]);
    assert_exit(&html, 0);
    let html = String::from_utf8(html.stdout).unwrap();
    assert!(html.contains("<strong>Denied</strong>"));
    assert!(html.contains("HIGH PG006"));
    assert!(!html.contains("UPDATE public.items SET id = 7"));

    let sarif = project.run(&["report", "--input", "report.json", "--format", "sarif"]);
    assert_exit(&sarif, 0);
    let sarif: Value = serde_json::from_slice(&sarif.stdout).unwrap();
    let run = &sarif["runs"][0];
    assert_eq!(sarif["version"], "2.1.0");
    assert_eq!(run["properties"]["dmgDecision"], "denied");
    assert_eq!(run["invocations"][0]["executionSuccessful"], true);
    let result = &run["results"][0];
    assert_eq!(result["ruleId"], "PG006");
    assert_eq!(
        result["locations"][0]["physicalLocation"]["artifactLocation"]["uri"],
        "migrations/002_update.sql"
    );

    let code_quality = project.run(&["report", "--input", "report.json", "--format", "gitlab"]);
    assert_exit(&code_quality, 0);
    let code_quality: Value = serde_json::from_slice(&code_quality.stdout).unwrap();
    assert_eq!(code_quality[0]["check_name"], "PG006");
    assert_eq!(code_quality[0]["severity"], "critical");
    assert_eq!(
        code_quality[0]["fingerprint"],
        result["fingerprints"]["dmgFinding/v1"]
    );

    project.write("bad-report.json", r#"{"schema_version":9}"#);
    let invalid = project.run(&[
        "report",
        "--input",
        "bad-report.json",
        "--format",
        "markdown",
        "--output",
        "summary.md",
    ]);
    assert_exit(&invalid, 2);
    assert_eq!(
        fs::read_to_string(project.path("summary.md")).unwrap(),
        summary
    );
}

#[test]
fn machine_review_rejects_invalid_source_paths_before_output() {
    let project = Fixture::new();
    project.write(
        "migrations/002_update.sql",
        "UPDATE public.items SET id = 7;",
    );
    let checked = project.check();
    assert_exit(&checked, 1);
    let mut saved = report(&checked);
    saved["findings"][0]["span"]["path"] = json!("migrations/line\nbreak.sql");
    project.write("report.json", serde_json::to_vec(&saved).unwrap());
    project.write("review.json", "previous review");

    for format in ["gitlab", "sarif"] {
        let args = ["report", "--input", "report.json", "--format", format];
        let stdout = project.run(&args);
        assert_exit(&stdout, 2);
        assert!(stdout.stdout.is_empty());
        assert!(!String::from_utf8_lossy(&stdout.stderr).contains("line"));

        let file = project.run(&[args.as_slice(), &["--output", "review.json"]].concat());
        assert_exit(&file, 2);
        assert_eq!(
            fs::read(project.path("review.json")).unwrap(),
            b"previous review"
        );
    }
}

#[test]
fn init_preserves_an_existing_file_and_writes_usable_config() {
    let project = Fixture::new();
    assert_exit(
        &project.run(&["init", "--target", "17", "--config", "starter.toml"]),
        0,
    );
    let original = fs::read(project.path("starter.toml")).unwrap();
    assert_exit(
        &project.run(&["init", "--target", "18", "--config", "starter.toml"]),
        2,
    );
    assert_eq!(fs::read(project.path("starter.toml")).unwrap(), original);
    let scan = project.run(&["scan", "--config", "starter.toml", "--format", "json"]);
    assert_exit(&scan, 0);
    assert_eq!(report(&scan)["target"], 17);
}

#[test]
fn validates_policy_and_usage_before_analysis() {
    let project = Fixture::new();
    let validate = |path| project.run(&["policy", "validate", "--policy", path]);
    assert_exit(&validate("policy.toml"), 0);
    project.write(
        "unknown.toml",
        "schema_version = 1\n[rules.PG999]\nseverity = 'high'\n",
    );
    let unknown = validate("unknown.toml");
    assert_exit(&unknown, 2);
    assert!(String::from_utf8_lossy(&unknown.stderr).contains("rule PG999 is unknown"));

    project.write(
        "invalid.toml",
        "schema_version = 9\ncredential = 'synthetic-private-value'\n",
    );
    let invalid = project.run(&[
        "check",
        "--policy",
        "invalid.toml",
        "--repo",
        "absent",
        "--format",
        "json",
    ]);
    assert_exit(&invalid, 2);
    assert!(invalid.stdout.is_empty());
    assert!(!String::from_utf8_lossy(&invalid.stderr).contains("synthetic-private-value"));
    assert_exit(&project.run(&["check"]), 2);
    assert_exit(&project.run(&["scan", "--base", &"a".repeat(40)]), 2);
    assert_exit(
        &project.run(&["scan", "--base", "HEAD", "--head", "HEAD"]),
        2,
    );
}

#[test]
fn scan_and_check_return_distinct_status_and_digest() {
    let project = Fixture::new();
    project.write(
        "migrations/002_update.sql",
        "UPDATE public.items SET id = 7; -- synthetic payload\n",
    );

    let scan = project.run(&["scan", "--format", "json"]);
    assert_exit(&scan, 0);
    let scanned = report(&scan);
    assert_eq!(scanned["schema_version"], 1);
    assert_eq!(scanned["coverage"]["status"], "complete");
    assert_eq!(scanned["decision"], Value::Null);
    assert_eq!(scanned["severity_summary"]["high"], 1);
    assert!(!String::from_utf8_lossy(&scan.stdout).contains("synthetic payload"));

    let check = project.check();
    assert_exit(&check, 1);
    let denied = report(&check);
    assert_eq!(denied["decision"]["status"], "denied");
    assert_eq!(
        denied["decision"]["policy_digest"],
        ContentDigest::of(POLICY.as_bytes()).to_string()
    );
    assert_eq!(
        denied["decision"]["reasons"][0]["finding_fingerprint"],
        denied["findings"][0]["fingerprint"]
    );
    assert_eq!(project.check().stdout, check.stdout);
}

#[test]
fn incomplete_evidence_takes_precedence_and_reports_survive_nonzero_exit() {
    let project = Fixture::new();
    project.write(
        "migrations/002_update.sql",
        "UPDATE public.items SET id = 7;\n",
    );
    let check = || {
        project.run(&[
            "check",
            "--policy",
            "policy.toml",
            "--format",
            "json",
            "--output",
            "report.json",
        ])
    };
    let first = check();
    assert_exit(&first, 1);
    assert!(first.stdout.is_empty());
    let denied = project.read_report();
    assert_eq!(denied["decision"]["status"], "denied");

    project.write(
        "policy.toml",
        "schema_version = 1\n[integrity]\nrequire_git_base = true\n",
    );
    let second = check();
    assert_exit(&second, 3);
    assert!(second.stdout.is_empty());
    let incomplete = project.read_report();
    assert_eq!(incomplete["decision"]["status"], "incomplete");
    assert_eq!(
        incomplete["decision"]["reasons"][0]["kind"],
        "required_evidence"
    );
    assert_eq!(incomplete["findings"], denied["findings"]);

    project.write(
        "migrations/003_incomplete.sql",
        "ALTER TABLE public.items ADD COLUMN;\n",
    );
    let third = check();
    assert_exit(&third, 3);
    let incomplete = project.read_report();
    assert_eq!(incomplete["coverage"]["status"], "incomplete");
    assert!(
        incomplete["decision"]["reasons"]
            .as_array()
            .unwrap()
            .iter()
            .any(|reason| reason["kind"] == "required_coverage")
    );
    assert!(!incomplete["findings"].as_array().unwrap().is_empty());

    project.write("policy.toml", "schema_version = 3\n");
    assert_exit(&check(), 2);
    assert_eq!(project.read_report(), incomplete);
    let unavailable_output =
        project.run(&["scan", "--format", "json", "--output", "absent/report.json"]);
    assert_exit(&unavailable_output, 3);
    assert!(unavailable_output.stdout.is_empty());
}

#[test]
fn severity_overrides_and_distinct_custom_exit_codes() {
    let project = Fixture::new();
    project.write(
        "dmg.toml",
        format!("{CONFIG}\n[exit_codes]\ndenied = 11\nincomplete = 13\n"),
    );
    project.write(
        "migrations/002_update.sql",
        "UPDATE public.items SET id = 7;\n",
    );
    assert_exit(&project.check(), 11);
    project.write(
        "policy.toml",
        "schema_version = 1\n[rules.PG006]\nseverity = 'medium'\n[rules.PG001]\nseverity = 'low'\n",
    );
    let downgraded = project.check();
    assert_exit(&downgraded, 0);
    assert_eq!(report(&downgraded)["severity_summary"]["medium"], 1);
    project.write("migrations/003_drop.sql", "DROP TABLE public.items;\n");
    let critical = project.check();
    assert_exit(&critical, 11);
    assert_eq!(report(&critical)["severity_summary"]["critical"], 1);

    project.write(
        "migrations/004_incomplete.sql",
        "ALTER TABLE public.items ADD COLUMN;\n",
    );
    assert_exit(&project.check(), 13);
    project.write(
        "dmg.toml",
        format!("{CONFIG}\n[exit_codes]\ndenied = 2\nincomplete = 2\n"),
    );
    assert_exit(&project.check(), 2);
}

#[test]
fn pinned_manifests_report_provenance_and_fail_closed() {
    let project = Fixture::new();
    let source = fs::read(project.path("migrations/001_create.sql")).unwrap();
    let digest = ContentDigest::of(&source);
    let integrity = json!({
        "schema_version": 1, "adapter": "plain",
        "migrations": [{"version": 1, "direction": "forward", "path": "migrations/001_create.sql", "sha256": digest}]
    }).to_string();
    let history = json!({
        "schema_version": 1, "adapter": "plain",
        "migrations": [{"version": 1, "status": "applied", "sha256": digest}]
    })
    .to_string();
    let integrity_digest = ContentDigest::of(integrity.as_bytes()).to_string();
    let history_digest = ContentDigest::of(history.as_bytes()).to_string();
    project.write("integrity.json", integrity);
    project.write("history.json", history);
    project.write("migrations/002_select.sql", "SELECT 1;\n");
    let check = |digest: &str| {
        project.run(&[
            "check",
            "--policy",
            "policy.toml",
            "--format",
            "json",
            "--integrity-manifest",
            "integrity.json",
            "--integrity-sha256",
            &integrity_digest,
            "--applied-manifest",
            "history.json",
            "--applied-sha256",
            digest,
        ])
    };
    let pass = check(&history_digest);
    assert_exit(&pass, 0);
    let passed = report(&pass);
    assert_eq!(
        passed["migrations"]["integrity"]["basis"]["kind"],
        "manifest"
    );
    assert_eq!(
        passed["migrations"]["applied_history"]["migrations"][0]["status"],
        "applied"
    );
    assert_eq!(
        passed["migrations"]["applied_history"]["migrations"][1]["status"],
        "pending"
    );

    let mismatch = check(&"0".repeat(64));
    assert_exit(&mismatch, 3);
    assert_eq!(
        report(&mismatch)["coverage"]["gaps"][0]["kind"],
        "missing_applied_history"
    );
    assert_eq!(
        report(&mismatch)["migrations"]["applied_history"],
        Value::Null
    );

    project.write(
        "policy.toml",
        "schema_version = 1\n[integrity]\nrequire_git_base = true\n",
    );
    let missing_git = check(&history_digest);
    assert_exit(&missing_git, 3);
    assert_eq!(
        report(&missing_git)["decision"]["reasons"][0]["kind"],
        "required_evidence"
    );
}

#[test]
fn text_escapes_controls_in_identifiers_and_prints_predicted_locks() {
    let project = Fixture::new();
    project.write(
        "migrations/001_create.sql",
        "CREATE TABLE public.\"café\nrecords\" (id integer);\n",
    );
    project.write(
        "migrations/002_update.sql",
        "UPDATE public.\"café\nrecords\" SET id = 7;\n",
    );
    project.write(
        "migrations/003_add.sql",
        "ALTER TABLE public.\"café\nrecords\" ADD COLUMN notes text;\n",
    );
    let scan = project.run(&["scan"]);
    assert_exit(&scan, 0);
    let text = String::from_utf8(scan.stdout).unwrap();
    assert!(
        text.contains("migrations/002_update.sql:1:1 HIGH PG006"),
        "{text}"
    );
    assert!(text.contains("public.café\\u{a}records"), "{text}");
    assert!(!text.contains("café\nrecords"), "{text}");
    assert!(text.contains("predicted ACCESS EXCLUSIVE lock"), "{text}");
}

#[test]
fn text_escapes_bidi_in_sqlx_paths() {
    let project = Fixture::new();
    project.write(
        "dmg.toml",
        r#"schema_version = 1
target = 18
roots = ["migrations"]

[adapter]
kind = "sqlx"
layout = "simple"
"#,
    );
    project.write(
        "migrations/002_\u{202e}update.sql",
        "UPDATE public.items SET id = 7;\n",
    );
    let scan = project.run(&["scan"]);
    assert_exit(&scan, 0);
    let text = String::from_utf8(scan.stdout).unwrap();
    assert!(
        text.contains("migrations/002_\\u{202e}update.sql:1:1"),
        "{text}"
    );
    assert!(!text.contains('\u{202e}'), "{text}");
}

#[test]
fn flyway_resolves_versions_repeatables_and_transaction_settings() {
    let project = Fixture::new();
    fs::remove_file(project.path("migrations/001_create.sql")).unwrap();
    project.write(
        "migrations/V01_02__base.SQL",
        "CREATE TABLE public.items (id integer);\n",
    );
    project.write(
        "migrations/V1.10__index.sql",
        "CREATE INDEX CONCURRENTLY items_idx ON public.items (id);\n",
    );
    project.write("migrations/R__a_check.sql", "SELECT 1;\n");
    project.write("dmg.toml", native_config("flyway", ""));
    assert_exit(&project.check(), 3);

    project.write(
        "dmg.toml",
        native_config("flyway", "transactional_lock = false\n"),
    );
    let checked = project.check();
    assert_exit(&checked, 0);
    let report = report(&checked);
    let files = report["migrations"]["inventory"].as_array().unwrap();
    assert_eq!(files.len(), 3);
    assert_eq!(
        files[0]["identity"]["version"],
        json!({"kind": "flyway", "value": "1.2"})
    );
    assert_eq!(files[0]["execution"]["transaction_wrapper"], "file");
    assert_eq!(files[1]["execution"]["transaction_wrapper"], "none");
    assert_eq!(
        files[2]["identity"]["version"],
        json!({"kind": "repeatable", "value": "a check"})
    );

    project.write("dmg.toml", native_config("flyway", "group = true\n"));
    assert_exit(&project.check(), 2);
}

#[test]
fn goose_reports_shared_directional_bytes_and_runner_query_groups() {
    let project = Fixture::new();
    project.write(
        "migrations/001_create.sql",
        "-- +goose Up\nCREATE TABLE public.items (id integer);\n\
         -- +goose Down\nDROP TABLE public.items;\n",
    );
    project.write(
        "migrations/002_index.sql",
        "-- +goose NO TRANSACTION\n-- +goose Up\n\
         ALTER TABLE public.items ADD COLUMN note text; CREATE INDEX CONCURRENTLY items_idx ON public.items (id);\n",
    );
    project.write("dmg.toml", native_config("goose", ""));
    let checked = project.check();
    assert_exit(&checked, 1);
    let report = report(&checked);
    let files = report["migrations"]["inventory"].as_array().unwrap();
    assert_eq!(files.len(), 3);
    assert_eq!(files[0]["direction"], "forward");
    assert_eq!(files[1]["direction"], "down");
    assert_eq!(files[0]["digest"], files[1]["digest"]);
    assert_eq!(files[0]["path"], files[1]["path"]);
    assert_eq!(files[2]["source_segments"].as_array().unwrap().len(), 1);
    assert_eq!(report["findings"][0]["rule_id"], "PG007");
    assert_eq!(report["findings"][0]["span"]["start"]["line"], 3);
}

#[test]
fn supabase_keeps_lexical_versions_and_preserves_native_batch_boundaries() {
    let project = Fixture::new();
    fs::remove_file(project.path("migrations/001_create.sql")).unwrap();
    project.write(
        "migrations/01_base.sql",
        "CREATE TABLE public.items (id integer);\n",
    );
    project.write("migrations/2_last.sql", "SELECT 1;\n");
    project.write("dmg.toml", native_config("supabase", ""));
    let prefix = "ALTER TABLE public.items ADD COLUMN note text;\n";
    project.write(
        "migrations/10_index.sql",
        format!("{prefix}CREATE INDEX CONCURRENTLY items_idx ON public.items (id);\n"),
    );
    let checked = project.check();
    assert_exit(&checked, 0);
    let data = report(&checked);
    let files = data["migrations"]["inventory"].as_array().unwrap();
    let versions = files
        .iter()
        .map(|file| file["identity"]["version"]["value"].as_str().unwrap())
        .collect::<Vec<_>>();
    assert_eq!(versions, ["01", "10", "2"]);

    project.write(
        "migrations/10_index.sql",
        format!("{prefix}CREATE /* internal comment */ INDEX CONCURRENTLY items_idx ON public.items (id);\n"),
    );
    let checked = project.check();
    assert_exit(&checked, 1);
    assert_eq!(report(&checked)["findings"][0]["rule_id"], "PG007");
}

#[test]
fn flyway_repeatable_edits_remain_selected_and_missing_repeatables_block() {
    let project = Fixture::new();
    fs::remove_file(project.path("migrations/001_create.sql")).unwrap();
    project.write("dmg.toml", native_config("flyway", ""));
    let path = "migrations/R__summary.sql";
    let original = b"SELECT 1;\n";
    let integrity = json!({
        "schema_version": 1,
        "adapter": "flyway",
        "migrations": [{
            "version": {"kind": "repeatable", "value": "summary"},
            "direction": "forward",
            "path": path,
            "sha256": ContentDigest::of(original)
        }]
    })
    .to_string();
    let digest = ContentDigest::of(integrity.as_bytes()).to_string();
    project.write("integrity.json", integrity);
    project.write(path, "SELECT 2;\n");

    let check = || {
        project.run(&[
            "check",
            "--policy",
            "policy.toml",
            "--format",
            "json",
            "--integrity-manifest",
            "integrity.json",
            "--integrity-sha256",
            &digest,
        ])
    };
    let changed = check();
    assert_exit(&changed, 0);
    let data = report(&changed);
    assert_eq!(data["migrations"]["selected_forwards"], json!([path]));
    assert_eq!(
        data["migrations"]["integrity"]["changes"][0]["status"],
        "modified"
    );

    fs::remove_file(project.path(path)).unwrap();
    let removed = check();
    assert_exit(&removed, 1);
    assert_eq!(
        report(&removed)["decision"]["reasons"][0]["kind"],
        "integrity_violation"
    );
}

fn native_config(adapter: &str, settings: &str) -> String {
    format!(
        "schema_version = 1\ntarget = 18\nroots = [\"migrations\"]\n[adapter]\nkind = \"{adapter}\"\n{settings}"
    )
}

struct Fixture(TempDir);

impl Fixture {
    fn new() -> Self {
        let fixture = Self(tempfile::tempdir().unwrap());
        fs::create_dir(fixture.path("migrations")).unwrap();
        fixture.write("dmg.toml", CONFIG);
        fixture.write("policy.toml", POLICY);
        fixture.write(
            "migrations/001_create.sql",
            "CREATE TABLE public.items (id integer);\n",
        );
        fixture
    }

    fn path(&self, relative: impl AsRef<Path>) -> PathBuf {
        self.0.path().join(relative)
    }

    fn write(&self, relative: &str, content: impl AsRef<[u8]>) {
        fs::write(self.path(relative), content).unwrap();
    }

    fn run(&self, args: &[&str]) -> Output {
        Command::new(env!("CARGO_BIN_EXE_dmg"))
            .current_dir(self.0.path())
            .args(args)
            .output()
            .unwrap()
    }

    fn check(&self) -> Output {
        self.run(&["check", "--policy", "policy.toml", "--format", "json"])
    }

    fn read_report(&self) -> Value {
        serde_json::from_slice(&fs::read(self.path("report.json")).unwrap()).unwrap()
    }
}

fn assert_exit(output: &Output, expected: i32) {
    assert_eq!(
        output.status.code(),
        Some(expected),
        "stdout\n{}\nstderr\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

fn report(output: &Output) -> Value {
    serde_json::from_slice(&output.stdout).unwrap()
}
