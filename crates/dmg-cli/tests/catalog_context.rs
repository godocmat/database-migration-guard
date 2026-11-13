use std::{
    fs,
    path::Path,
    process::{Command, Output},
    time::{SystemTime, UNIX_EPOCH},
};

use dmg_core::{digest::ContentDigest, inspection::InspectionEvidenceV1};
use serde_json::{Value, json};
use tempfile::TempDir;

const PROJECT: &str = r#"schema_version = 1
target = 18
roots = ["migrations"]

[adapter]
kind = "plain"
filename_pattern = '^([0-9]+)_[a-z][a-z0-9_]*\.sql$'
version_order = "positive_integer"
transaction = "autocommit"
"#;
const TARGET: &str = "fixture-catalog";
const PRIVATE_DEFINITION: &str = "private_catalog_definition";
const PRIVATE_DEPENDENT: &str = "private_dependency";

fn fixture(sql: &str) -> TempDir {
    let fixture = tempfile::tempdir().unwrap();
    fs::write(fixture.path().join("dmg.toml"), PROJECT).unwrap();
    fs::write(fixture.path().join("policy.toml"), "schema_version = 1\n").unwrap();
    let migrations = fixture.path().join("migrations");
    fs::create_dir(&migrations).unwrap();
    fs::write(migrations.join("1_review.sql"), sql).unwrap();
    fixture
}

fn run(directory: &Path, args: &[&str], expected_exit: i32) -> Output {
    let output = Command::new(env!("CARGO_BIN_EXE_dmg"))
        .current_dir(directory)
        .args(args)
        .output()
        .unwrap();
    assert_eq!(
        output.status.code(),
        Some(expected_exit),
        "args {args:?}\nstdout: {}\nstderr: {}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    output
}

fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs()
}

fn artifact() -> Value {
    json!({
        "schema_version": 1,
        "observation": {
            "target_id": TARGET, "server_version_num": 180007, "observed_at_unix_seconds": now()
        },
        "scope": [
            {"schema": "public", "state": "visible"},
            {"schema": "private", "state": "denied"}
        ],
        "snapshot": {
            "schema_version": 1,
            "schemas": ["public"],
            "tables": [{
                "name": {"schema": "public", "name": "items"},
                "kind": "ordinary",
                "is_partition": false,
                "columns": [{
                    "name": "id", "position": 1,
                    "data_type": {"schema": "pg_catalog", "name": "int8"},
                    "type_modifier": -1, "array_dimensions": 0,
                    "not_null": true, "identity": null, "generation": null,
                    "has_default": true,
                    "default_definition": {"state": "observed", "value": PRIVATE_DEFINITION}
                }],
                "indexes": [],
                "constraints": []
            }],
            "partition_edges": [],
            "dependencies": [{
                "dependent": {"kind": "relation", "schema": "public", "name": PRIVATE_DEPENDENT, "member": null},
                "referenced": {"kind": "relation", "schema": "public", "name": "items", "member": null},
                "kind": "normal"
            }],
            "gaps": [{"scope": {"kind": "target"}, "field": "columns", "reason": "truncated"}]
        },
        "relation_estimates": [{
            "relation": {"schema": "public", "name": "items"},
            "rows": {"state": "estimated", "value": 0},
            "heap_bytes": {"state": "estimated", "value": 8192},
            "total_bytes": {"state": "observed", "value": 24576}
        }],
        "history": null,
        "gaps": [
            {"scope": {"kind": "schema", "schema": "private"}, "field": "scope", "reason": "permission_denied"},
            {"scope": {"kind": "target"}, "field": "dependencies", "reason": "outside_scope"}
        ]
    })
}

fn save(directory: &Path, evidence: &Value) -> ContentDigest {
    // Verify the fixture matches the format written by inspect before testing the importer.
    let _: InspectionEvidenceV1 = serde_json::from_value(evidence.clone()).unwrap();
    let bytes = serde_json::to_vec_pretty(evidence).unwrap();
    fs::write(directory.join("catalog.json"), &bytes).unwrap();
    ContentDigest::of(&bytes)
}

fn with_catalog(
    directory: &Path,
    pin: ContentDigest,
    target: &str,
    args: &[&str],
    exit: i32,
) -> Value {
    let pin = pin.to_string();
    let mut arguments = args.to_vec();
    arguments.extend([
        "--catalog-evidence",
        "catalog.json",
        "--catalog-evidence-sha256",
        &pin,
        "--catalog-target-id",
        target,
        "--catalog-max-age-seconds",
        "600",
        "--format",
        "json",
    ]);
    serde_json::from_slice(&run(directory, &arguments, exit).stdout).unwrap()
}

#[test]
fn catalog_reports_positive_facts_and_preserves_static_decisions() {
    let fixture = fixture("TRUNCATE TABLE public.items;\n");
    let directory = fixture.path();
    let pin = save(directory, &artifact());
    let check = ["check", "--policy", "policy.toml"];
    let report = with_catalog(directory, pin, TARGET, &check, 1);
    let offline = run(
        directory,
        &["check", "--policy", "policy.toml", "--format", "json"],
        1,
    );
    let offline: Value = serde_json::from_slice(&offline.stdout).unwrap();
    assert_eq!(report["decision"], offline["decision"]);
    assert_eq!(report["coverage"], offline["coverage"]);
    assert_eq!(report["relation_locks"], offline["relation_locks"]);
    assert!(offline.get("catalog_context").is_none());

    let audit = &report["catalog_context"];
    assert_eq!(audit["artifact_digest"], json!(pin));
    assert_eq!(audit["observation"]["target_id"], TARGET);
    assert_eq!(audit["coverage"], "incomplete");
    assert_eq!(audit["scope"][0]["state"], "denied");
    assert!(
        audit["gaps"]
            .as_array()
            .unwrap()
            .contains(&json!({"field": "columns", "reason": "truncated"}))
    );

    let finding = &report["findings"][0];
    assert_eq!(finding["severity"], "critical");
    assert_eq!(
        finding["fingerprint"],
        offline["findings"][0]["fingerprint"]
    );
    let facts = finding["evidence"].as_array().unwrap();
    for (kind, phrase) in [
        ("input_fact", "an incoming dependency"),
        ("input_fact", "24576 bytes"),
        ("catalog_estimate", "estimate of 0 rows"),
        ("catalog_estimate", "8192 bytes"),
    ] {
        assert!(
            facts.iter().any(|fact| {
                fact["artifact_digest"] == json!(pin)
                    && fact["kind"] == kind
                    && fact["detail"].as_str().unwrap().contains(phrase)
            }),
            "{finding}"
        );
    }
    let rendered = report.to_string();
    assert!(!rendered.contains(PRIVATE_DEFINITION));
    assert!(!rendered.contains(PRIVATE_DEPENDENT));
    fs::write(directory.join("report.json"), rendered).unwrap();
    for format in ["markdown", "html"] {
        let output = run(
            directory,
            &["report", "--input", "report.json", "--format", format],
            0,
        );
        let output = String::from_utf8(output.stdout).unwrap();
        assert!(output.contains("Catalog coverage"));
        assert!(output.contains(&pin.to_string()));
        assert!(output.contains("estimate of 0 rows"));
        assert!(!output.contains(PRIVATE_DEFINITION));
        assert!(!output.contains(PRIVATE_DEPENDENT));
    }
}

#[test]
fn rejects_untrusted_catalog_artifacts_and_conflicting_targets() {
    let fixture = fixture("TRUNCATE TABLE public.items;\n");
    let directory = fixture.path();
    let evidence = artifact();
    let pin = save(directory, &evidence);
    let scan = ["scan"];
    let mut results = vec![
        (
            with_catalog(directory, pin, "wrong-target", &scan, 3),
            "different target",
        ),
        (
            with_catalog(
                directory,
                ContentDigest::of(b"other bytes"),
                TARGET,
                &scan,
                3,
            ),
            "does not match its trusted pin",
        ),
    ];
    for (field, value, message) in [
        ("server_version_num", json!(170012), "server major differs"),
        ("observed_at_unix_seconds", json!(1), "older than"),
        (
            "observed_at_unix_seconds",
            json!(now() + 10_000),
            "in the future",
        ),
    ] {
        let mut altered = evidence.clone();
        altered["observation"][field] = value;
        let pin = save(directory, &altered);
        results.push((with_catalog(directory, pin, TARGET, &scan, 3), message));
    }

    let malformed = b"{";
    fs::write(directory.join("catalog.json"), malformed).unwrap();
    results.push((
        with_catalog(directory, ContentDigest::of(malformed), TARGET, &scan, 3),
        "invalid versioned catalog JSON",
    ));
    let pin = save(directory, &evidence);
    fs::write(
        directory.join("policy.toml"),
        "schema_version = 1\n[evidence]\nrepository_id = 'fixture'\ntarget_id = 'other-policy-target'\n",
    )
    .unwrap();
    let check = ["check", "--policy", "policy.toml"];
    let policy_mismatch = with_catalog(directory, pin, TARGET, &check, 3);
    assert_eq!(policy_mismatch["decision"]["status"], "incomplete");
    results.push((policy_mismatch, "differs from the protected policy"));
    results.push((
        with_catalog(
            directory,
            pin,
            TARGET,
            &[
                "scan",
                "--native-history",
                "missing-history.json",
                "--native-history-sha256",
                &pin.to_string(),
                "--target-id",
                "other-history-target",
            ],
            3,
        ),
        "differs from native history",
    ));
    for (report, message) in results {
        assert!(report.get("catalog_context").is_none());
        assert_eq!(report["coverage"]["status"], "incomplete");
        assert!(
            report["coverage"]["gaps"]
                .as_array()
                .unwrap()
                .iter()
                .any(|gap| {
                    gap["kind"] == "missing_catalog_evidence"
                        && gap["message"].as_str().unwrap().contains(message)
                }),
            "{report}"
        );
        assert!(
            report["findings"][0]["evidence"]
                .as_array()
                .unwrap()
                .iter()
                .all(|fact| { fact["artifact_digest"].is_null() })
        );
    }
    run(
        directory,
        &["scan", "--catalog-evidence", "catalog.json"],
        2,
    );
    run(directory, &["scan", "--catalog-max-age-seconds", "600"], 2);
}

#[test]
fn catalog_attribution_stays_qualified_and_respects_migration_order() {
    for (sql, observed, sized) in [
        ("TRUNCATE TABLE items;\n", false, false),
        ("TRUNCATE TABLE private.items;\n", false, false),
        (
            "CREATE TABLE IF NOT EXISTS public.items (id bigint);\nTRUNCATE TABLE public.items;\n",
            false,
            false,
        ),
        (
            "CREATE TABLE private.items (id bigint);\nTRUNCATE TABLE public.items;\n",
            true,
            true,
        ),
        (
            "ALTER TABLE public.other RENAME TO items;\nTRUNCATE TABLE public.items;\n",
            false,
            false,
        ),
        (
            "UPDATE public.other SET id = 1;\nTRUNCATE TABLE public.items;\n",
            true,
            false,
        ),
    ] {
        let fixture = fixture(sql);
        let directory = fixture.path();
        let pin = save(directory, &artifact());
        let report = with_catalog(directory, pin, TARGET, &["scan"], 0);
        let finding = report["findings"]
            .as_array()
            .unwrap()
            .iter()
            .find(|finding| finding["rule_id"] == "PG001")
            .unwrap();
        assert_eq!(finding["severity"], "critical");
        let facts = finding["evidence"].as_array().unwrap();
        assert_eq!(
            facts.iter().any(|fact| fact["kind"] == "input_fact"),
            observed,
            "{sql}"
        );
        assert_eq!(
            facts.iter().any(|fact| fact["kind"] == "catalog_estimate"),
            sized,
            "{sql}"
        );
    }
}

#[test]
fn unavailable_catalog_preserves_coverage_diagnostics_without_facts() {
    let fixture = fixture("TRUNCATE TABLE public.items;\n");
    let directory = fixture.path();
    let mut evidence = artifact();
    evidence["observation"]["server_version_num"] = Value::Null;
    evidence["scope"] = json!([{"schema": "public", "state": "unavailable"}]);
    evidence["snapshot"] = Value::Null;
    evidence["relation_estimates"] = json!([]);
    evidence["gaps"] = json!([{
        "scope": {"kind": "target"}, "field": "connection", "reason": "deadline"
    }]);
    let pin = save(directory, &evidence);
    let report = with_catalog(directory, pin, TARGET, &["scan"], 3);
    assert_eq!(report["coverage"]["status"], "incomplete");
    assert_eq!(
        report["coverage"]["gaps"][0]["kind"],
        "missing_catalog_evidence"
    );
    let context = &report["catalog_context"];
    assert_eq!(context["coverage"], "incomplete");
    assert_eq!(context["artifact_digest"], json!(pin));
    assert_eq!(
        context["gaps"][0],
        json!({"field": "connection", "reason": "deadline"})
    );
    assert!(
        report["findings"][0]["evidence"]
            .as_array()
            .unwrap()
            .iter()
            .all(|fact| { fact["artifact_digest"].is_null() })
    );
}
