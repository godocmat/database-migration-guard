use std::{
    fs,
    path::Path,
    process::{Command, Output},
};

use serde_json::{Value, json};
use tempfile::TempDir;

const SECRET: &str = "synthetic-definition-value";

fn table(name: &str, column: &str, data_type: &str) -> Value {
    json!({
        "name": {"schema": "public", "name": name},
        "kind": "ordinary",
        "is_partition": false,
        "columns": [{
            "name": column,
            "position": 1,
            "data_type": {"schema": "pg_catalog", "name": data_type},
            "type_modifier": -1,
            "array_dimensions": 0,
            "not_null": false,
            "identity": null,
            "generation": null,
            "has_default": true,
            "default_definition": {"state": "observed", "value": format!("'{SECRET}'::text")}
        }],
        "indexes": [],
        "constraints": []
    })
}

fn snapshot(tables: Vec<Value>) -> Value {
    json!({
        "schema_version": 1,
        "schemas": ["public"],
        "tables": tables,
        "partition_edges": [],
        "dependencies": [],
        "gaps": []
    })
}

fn inspection(state: &str, snapshot: Option<Value>) -> Value {
    json!({
        "schema_version": 1,
        "observation": {
            "target_id": "staging",
            "server_version_num": 180007,
            "observed_at_unix_seconds": 1000
        },
        "scope": [{"schema": "public", "state": state}],
        "snapshot": snapshot,
        "relation_estimates": [],
        "history": null,
        "gaps": []
    })
}

fn contract(application: &str, column: &str) -> Value {
    json!({
        "schema_version": 1,
        "application": application,
        "version": "1.2",
        "critical": true,
        "tables": [{
            "table": {"schema": "public", "name": "Items"},
            "columns": [{
                "name": column,
                "access": "read",
                "data_type": {
                    "name": {"schema": "pg_catalog", "name": "text"},
                    "type_modifier": -1,
                    "array_dimensions": 0
                }
            }]
        }]
    })
}

fn rename(from: &str, to: &str) -> Value {
    json!({
        "schema_version": 1,
        "renames": [{
            "from": {"kind": "column", "schema": "public", "name": "Items", "member": from},
            "to": {"kind": "column", "schema": "public", "name": "Items", "member": to}
        }]
    })
}

fn save(directory: &Path, name: &str, value: Value) {
    fs::write(directory.join(name), serde_json::to_vec(&value).unwrap()).unwrap();
}

fn diff(directory: &Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_dmg"))
        .current_dir(directory)
        .args([
            "diff",
            "--expected",
            "expected.json",
            "--current",
            "current.json",
            "--target",
            "target.json",
        ])
        .args(args)
        .output()
        .unwrap()
}

fn assert_exit(output: &Output, exit: i32) {
    assert_eq!(
        output.status.code(),
        Some(exit),
        "stdout {}\nstderr {}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

fn report(output: &Output) -> Value {
    assert!(output.stderr.is_empty());
    assert!(!String::from_utf8_lossy(&output.stdout).contains(SECRET));
    serde_json::from_slice(&output.stdout).unwrap()
}

#[test]
fn compares_mixed_inputs_and_limits_rename_mappings_to_the_reviewed_step() {
    let scratch = TempDir::new().unwrap();
    let root = scratch.path();
    save(
        root,
        "expected.json",
        snapshot(vec![table("Items", "Label", "text")]),
    );
    save(
        root,
        "current.json",
        inspection(
            "visible",
            Some(snapshot(vec![table("Items", "Note", "text")])),
        ),
    );
    save(
        root,
        "target.json",
        snapshot(vec![table("Items", "Notes", "text")]),
    );
    save(root, "drift.json", rename("Label", "Note"));
    save(root, "proposed.json", rename("Note", "Notes"));

    let unmapped = diff(root, &["--format", "json"]);
    assert_exit(&unmapped, 0);
    let json = report(&unmapped);
    for pair in ["expected_to_current", "current_to_target"] {
        let kinds: Vec<_> = json[pair]["changes"]
            .as_array()
            .unwrap()
            .iter()
            .map(|change| change["kind"].as_str().unwrap())
            .collect();
        assert!(kinds.contains(&"added") && kinds.contains(&"removed"));
        assert!(!kinds.contains(&"renamed"));
    }
    assert_eq!(
        json["expected_to_current"]["after"]["observation"]["target_id"],
        "staging"
    );

    let args = [
        "--format",
        "json",
        "--drift-renames",
        "drift.json",
        "--proposed-renames",
        "proposed.json",
    ];
    let mapped = diff(root, &args);
    assert_exit(&mapped, 0);
    let json = report(&mapped);
    assert_eq!(json["schema_version"], 1);
    assert_eq!(json["coverage"], "complete");
    assert_eq!(json["expected_to_current"]["changes"][0]["kind"], "renamed");
    assert_eq!(
        json["current_to_target"]["changes"][0]["to"]["identity"]["member"],
        "Notes"
    );
    assert_eq!(mapped.stdout, diff(root, &args).stdout);

    let invalid = diff(root, &["--drift-renames", "proposed.json"]);
    assert_exit(&invalid, 2);
    assert!(invalid.stdout.is_empty());
    assert!(String::from_utf8_lossy(&invalid.stderr).contains("rename mapping"));
}

#[test]
fn reports_contract_regressions_after_reviewed_rename_and_sorts_contracts() {
    let scratch = TempDir::new().unwrap();
    let root = scratch.path();
    for name in ["expected.json", "current.json"] {
        save(root, name, snapshot(vec![table("Items", "Note", "text")]));
    }
    save(
        root,
        "target.json",
        snapshot(vec![table("Items", "note", "text")]),
    );
    save(root, "proposed.json", rename("Note", "note"));
    save(root, "old.json", contract("zeta", "Note"));
    save(root, "new.json", contract("alpha", "note"));

    let result = diff(
        root,
        &[
            "--format",
            "json",
            "--proposed-renames",
            "proposed.json",
            "--application-contract",
            "old.json",
            "--application-contract",
            "new.json",
        ],
    );
    assert_exit(&result, 0);
    let json = report(&result);
    let contracts = json["application_contracts"].as_array().unwrap();
    assert_eq!(contracts[0]["application"], "alpha");
    assert_eq!(contracts[1]["application"], "zeta");
    assert_eq!(contracts[1]["current"]["status"], "satisfied");
    assert_eq!(contracts[1]["target"]["status"], "violated");
    assert_eq!(contracts[1]["critical_target_violation"], true);
    assert!(
        contracts[1]["checks"]
            .as_array()
            .unwrap()
            .iter()
            .any(|check| {
                check["column"] == "Note"
                    && check["transition"] == "regression"
                    && check["target"]["reason"]["kind"] == "missing_column"
            })
    );
    assert_eq!(json["current_to_target"]["changes"][0]["kind"], "renamed");

    let text = diff(root, &["--application-contract", "old.json"]);
    assert_exit(&text, 0);
    let text = String::from_utf8(text.stdout).unwrap();
    assert!(text.contains("Critical target violation."));
    assert!(text.contains("\"public\".\"Items\".\"Note\" column type"));
    assert!(text.contains("regression"));
    assert!(!text.contains(SECRET));
}

#[test]
fn saves_an_incomplete_report_with_known_changes_and_unknown_contract_facts() {
    let scratch = TempDir::new().unwrap();
    let root = scratch.path();
    for name in ["expected.json", "current.json"] {
        save(
            root,
            name,
            snapshot(vec![
                table("Items", "Note", "text"),
                table("More", "id", "text"),
            ]),
        );
    }
    let mut partial = snapshot(vec![table("Items", "Note", "int8")]);
    partial["gaps"] = json!([{
        "scope": {"kind": "target"}, "field": "relations", "reason": "truncated"
    }]);
    save(root, "target.json", inspection("visible", Some(partial)));
    let mut application = contract("viewer", "Note");
    application["tables"].as_array_mut().unwrap().push(json!({
        "table": {"schema": "public", "name": "More"}
    }));
    save(root, "app.json", application);

    let result = diff(
        root,
        &[
            "--application-contract",
            "app.json",
            "--format",
            "json",
            "--output",
            "result.json",
        ],
    );
    assert_exit(&result, 3);
    assert!(result.stdout.is_empty() && result.stderr.is_empty());
    let bytes = fs::read(root.join("result.json")).unwrap();
    assert!(!String::from_utf8_lossy(&bytes).contains(SECRET));
    let json: Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(json["coverage"], "incomplete");
    let changes = json["current_to_target"]["changes"].as_array().unwrap();
    assert_eq!(changes.len(), 1);
    assert_eq!(changes[0]["kind"], "modified");
    assert_eq!(changes[0]["fields"], json!(["data_type"]));
    assert_eq!(
        json["application_contracts"][0]["target"]["coverage"],
        "incomplete"
    );
    assert_eq!(
        json["application_contracts"][0]["critical_target_violation"],
        true
    );
    assert!(
        json["application_contracts"][0]["checks"]
            .as_array()
            .unwrap()
            .iter()
            .any(|check| {
                check["table"]["name"] == "More" && check["target"]["status"] == "unknown"
            })
    );
}

#[test]
fn reviews_saved_diff_without_reclassifying_coverage_or_inserting_markup() {
    let scratch = TempDir::new().unwrap();
    let root = scratch.path();
    save(
        root,
        "expected.json",
        snapshot(vec![table("Items", "Note", "text")]),
    );
    save(
        root,
        "current.json",
        snapshot(vec![table("Items", "Note", "text")]),
    );

    let mut tables = vec![table("Items", "Note", "int8")];
    tables.extend((0..21).map(|index| table(&format!("extra_{index:02}"), "id", "text")));
    tables.push(table("<img src=x>", "id", "text"));
    let mut partial = snapshot(tables);
    partial["gaps"] = json!([{
        "scope": {"kind": "target"}, "field": "relations", "reason": "truncated"
    }]);
    save(root, "target.json", inspection("visible", Some(partial)));
    save(root, "app.json", contract("viewer", "Note"));

    let report = diff(
        root,
        &[
            "--application-contract",
            "app.json",
            "--format",
            "json",
            "--output",
            "result.json",
        ],
    );
    assert_exit(&report, 3);

    let review = |kind: &str, format: &str, output: &str| {
        Command::new(env!("CARGO_BIN_EXE_dmg"))
            .current_dir(root)
            .args([
                "report",
                "--kind",
                kind,
                "--input",
                "result.json",
                "--format",
                format,
                "--output",
                output,
            ])
            .output()
            .unwrap()
    };
    let output = review("diff", "html", "review.html");
    assert_exit(&output, 0);
    assert!(output.stdout.is_empty() && output.stderr.is_empty());
    let html = fs::read_to_string(root.join("review.html")).unwrap();
    assert!(html.contains("Content-Security-Policy"));
    assert!(html.contains("<h3>Drift (expected to current)</h3>"));
    assert!(html.contains("Schema comparison is incomplete."));
    assert!(html.contains("Critical target violation."));
    assert!(html.contains("3 more entries in JSON output."));
    assert!(html.contains("&lt;img src=x&gt;") && !html.contains("<img"));
    assert!(!html.contains(SECRET));

    let output = review("diff", "markdown", "review.md");
    assert_exit(&output, 0);
    let markdown = fs::read_to_string(root.join("review.md")).unwrap();
    assert!(markdown.contains("### Proposed changes (current to target)"));
    assert!(markdown.contains("regression"));
    assert!(markdown.contains(r"\<img src=x\>"));
    assert!(!markdown.contains(SECRET));

    let report = fs::read_to_string(root.join("result.json")).unwrap();
    for (kind, format, content) in [
        ("diff", "sarif", report.clone()),
        ("rehearsal", "html", report.clone()),
        (
            "diff",
            "html",
            report.replace(
                r#""application_contracts": ["#,
                r#""application_contracts": [], "application_contracts": ["#,
            ),
        ),
        (
            "diff",
            "html",
            report.replacen(r#""schema_version": 1"#, r#""schema_version": 2"#, 1),
        ),
        ("diff", "html", {
            let mut inconsistent: Value = serde_json::from_str(&report).unwrap();
            inconsistent["coverage"] = json!("complete");
            inconsistent.to_string()
        }),
    ] {
        fs::write(root.join("result.json"), content).unwrap();
        let invalid = review(kind, format, "review.html");
        assert_exit(&invalid, 2);
        assert!(invalid.stdout.is_empty());
        assert_eq!(fs::read_to_string(root.join("review.html")).unwrap(), html);
    }
}

#[test]
fn rejects_mixed_or_duplicate_shape_fields_without_replacing_output() {
    let scratch = TempDir::new().unwrap();
    let root = scratch.path();
    let observed = snapshot(vec![table("Items", "Note", "text")]);
    save(root, "expected.json", observed.clone());
    save(root, "current.json", observed.clone());
    fs::write(root.join("result.json"), "existing").unwrap();

    let mut mixed = observed;
    mixed["observation"] = json!({"connection_string": SECRET});
    save(root, "target.json", mixed);
    let args = ["--format", "json", "--output", "result.json"];
    let invalid = diff(root, &args);
    assert_exit(&invalid, 2);
    assert!(!String::from_utf8_lossy(&invalid.stderr).contains(SECRET));
    assert!(invalid.stdout.is_empty());
    assert_eq!(fs::read(root.join("result.json")).unwrap(), b"existing");

    fs::write(root.join("target.json"), br#"{"schema_version":1,"schemas":["public"],"tables":[],"tables":[],"partition_edges":[],"dependencies":[],"gaps":[]}"#).unwrap();
    assert_exit(&diff(root, &args), 2);
    assert_eq!(fs::read(root.join("result.json")).unwrap(), b"existing");

    fs::remove_file(root.join("target.json")).unwrap();
    assert_exit(&diff(root, &args), 3);
    assert_eq!(fs::read(root.join("result.json")).unwrap(), b"existing");
}
