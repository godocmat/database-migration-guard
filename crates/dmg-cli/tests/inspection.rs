use std::{
    fs,
    path::Path,
    process::{Command, Output},
};

use dmg_core::{
    digest::ContentDigest,
    history::{
        NativeChecksum, NativeHistoryCoverage, NativeHistoryEvidenceV1, NativeHistoryRow,
        NativeHistoryState, Sha384Digest, forward_inventory_digest,
    },
    inspection::{
        InspectionField, InspectionGap, InspectionGapReason, InspectionGapScope, Observation,
        TargetId,
    },
    migration::{MigrationAdapter, MigrationDirection, MigrationIdentity},
    snapshot::QualifiedName,
    source::RepoPath,
};
use dmg_discovery::{Adapter, DiscoveryLimits, DiscoveryOptions, SqlxLayout, discover_worktree};
use serde_json::{Value, json};

const MIGRATION: &[u8] = b"CREATE TABLE public.items (id integer);\n";
const PROJECT: &str = r#"schema_version = 1
target = 18
roots = ["migrations"]

[adapter]
kind = "sqlx"
layout = "simple"
"#;
const TARGET_ID: &str = "fixture-a";
const CONNECTION: &str = r#"schema_version = 1
target_id = "fixture-a"
target = 18
schemas = ["public"]
host = "private-host.invalid"
port = 5432
database = "private-database"
user = "private-user"
ca_file = "private-ca.pem"
password_file = "private-password.bin"
"#;

fn run(directory: &Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_dmg"))
        .current_dir(directory)
        .args(args)
        .output()
        .unwrap()
}

fn assert_exit(output: &Output, expected: i32) {
    assert_eq!(
        output.status.code(),
        Some(expected),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
}

fn inventory(directory: &Path) -> ContentDigest {
    let options = DiscoveryOptions::new(
        vec![RepoPath::new("migrations").unwrap()],
        Adapter::Sqlx(SqlxLayout::Simple),
        DiscoveryLimits::default(),
    )
    .unwrap();
    let discovery = discover_worktree(directory, &options);
    assert!(discovery.head_complete());
    forward_inventory_digest(discovery.adapter(), &discovery.plan.inventory).unwrap()
}

fn evidence(
    inventory_digest: ContentDigest,
    server_version: Option<u32>,
) -> NativeHistoryEvidenceV1 {
    let (coverage, rows, gaps) = if server_version.is_some() {
        (
            NativeHistoryCoverage::Complete,
            vec![NativeHistoryRow {
                identity: MigrationIdentity::numeric(MigrationAdapter::Sqlx, 1),
                direction: MigrationDirection::Forward,
                rank: None,
                state: NativeHistoryState::Applied,
                checksum: Some(NativeChecksum::SqlxSha384 {
                    digest: Sha384Digest::of(MIGRATION),
                }),
            }],
            Vec::new(),
        )
    } else {
        (
            NativeHistoryCoverage::Unavailable,
            Vec::new(),
            vec![InspectionGap {
                scope: InspectionGapScope::Target,
                field: InspectionField::Connection,
                reason: InspectionGapReason::Deadline,
            }],
        )
    };
    NativeHistoryEvidenceV1::new(
        Observation {
            target_id: TargetId::new(TARGET_ID).unwrap(),
            server_version_num: server_version,
            observed_at_unix_seconds: 1,
        },
        MigrationAdapter::Sqlx,
        Some(QualifiedName {
            schema: "public".into(),
            name: "_sqlx_migrations".into(),
        }),
        inventory_digest,
        coverage,
        rows,
        gaps,
    )
    .unwrap()
}

fn save_history(directory: &Path, evidence: &NativeHistoryEvidenceV1) -> ContentDigest {
    let mut bytes = serde_json::to_vec_pretty(evidence).unwrap();
    bytes.push(b'\n');
    fs::write(directory.join("history.json"), &bytes).unwrap();
    ContentDigest::of(&bytes)
}

fn check(directory: &Path, digest: ContentDigest, target_id: &str, exit: i32) -> Value {
    let output = run(
        directory,
        &[
            "check",
            "--policy",
            "policy.toml",
            "--format",
            "json",
            "--native-history",
            "history.json",
            "--native-history-sha256",
            &digest.to_string(),
            "--target-id",
            target_id,
        ],
    );
    assert_exit(&output, exit);
    serde_json::from_slice(&output.stdout).unwrap()
}

#[test]
fn checks_history_only_with_matching_pin_target_server_major_and_candidate() {
    let fixture = tempfile::tempdir().unwrap();
    let directory = fixture.path();
    fs::create_dir(directory.join("migrations")).unwrap();
    fs::write(directory.join("dmg.toml"), PROJECT).unwrap();
    fs::write(directory.join("policy.toml"), "schema_version = 1\n").unwrap();
    let migration = directory.join("migrations/1_create.sql");
    fs::write(&migration, MIGRATION).unwrap();
    fs::write(directory.join("migrations/2_pending.sql"), b"SELECT 1;\n").unwrap();

    let inventory = inventory(directory);
    let saved = evidence(inventory, Some(180007));
    let pin = save_history(directory, &saved);
    let passing = check(directory, pin, TARGET_ID, 0);
    let history = &passing["migrations"]["applied_history"];
    assert_eq!(
        history["basis"],
        json!({
            "kind": "native",
            "adapter": "sqlx",
            "digest": saved.digest().unwrap(),
            "inventory_digest": inventory,
            "method": "runner_checksum",
        })
    );
    assert_eq!(history["migrations"][0]["status"], "applied");
    assert_eq!(history["migrations"][1]["status"], "pending");
    assert!(!passing.to_string().contains("_sqlx_migrations"));

    let wrong_digest = check(directory, saved.digest().unwrap(), TARGET_ID, 3);
    let wrong_target = check(directory, pin, "fixture-b", 3);
    fs::write(&migration, b"CREATE TABLE public.other (id integer);\n").unwrap();
    let wrong_inventory = check(directory, pin, TARGET_ID, 3);
    fs::write(&migration, MIGRATION).unwrap();
    let wrong_major_pin = save_history(directory, &evidence(inventory, Some(170012)));
    let wrong_major = check(directory, wrong_major_pin, TARGET_ID, 3);
    for (report, expected_message) in [
        (wrong_digest, "does not match its trusted pin"),
        (wrong_target, "belongs to a different target"),
        (wrong_inventory, "different forward inventory"),
        (wrong_major, "server major differs"),
    ] {
        assert!(report["migrations"]["applied_history"].is_null());
        assert_eq!(
            report["coverage"]["gaps"][0]["kind"],
            "missing_applied_history"
        );
        assert!(
            report["coverage"]["gaps"][0]["message"]
                .as_str()
                .unwrap()
                .contains(expected_message)
        );
    }

    let unavailable_pin = save_history(directory, &evidence(inventory, None));
    let unavailable = check(directory, unavailable_pin, TARGET_ID, 3);
    assert_eq!(
        unavailable["migrations"]["applied_history"]["migrations"][0]["status"],
        "unknown"
    );
    assert_eq!(unavailable["decision"]["status"], "incomplete");
}

#[test]
fn exact_git_candidate_controls_history_discovery_and_import() {
    let fixture = tempfile::tempdir().unwrap();
    let directory = fixture.path();
    let git = |args: &[&str]| {
        let output = Command::new("git")
            .args(args)
            .current_dir(directory)
            .output()
            .unwrap();
        assert_exit(&output, 0);
        String::from_utf8(output.stdout).unwrap().trim().to_owned()
    };
    git(&["init", "-q"]);
    git(&["config", "user.name", "Migration Guard Fixture"]);
    git(&["config", "user.email", "fixture@example.invalid"]);
    git(&["config", "core.autocrlf", "false"]);
    git(&["config", "commit.gpgsign", "false"]);

    let migrations = directory.join("migrations");
    fs::create_dir(&migrations).unwrap();
    fs::write(migrations.join("1_create.sql"), MIGRATION).unwrap();
    git(&["add", "migrations"]);
    git(&["commit", "-qm", "base"]);
    let base = git(&["rev-parse", "HEAD"]);
    fs::write(migrations.join("2_pending.sql"), b"SELECT 1;\n").unwrap();
    git(&["add", "migrations"]);
    git(&["commit", "-qm", "head"]);
    let head = git(&["rev-parse", "HEAD"]);

    fs::write(directory.join("dmg.toml"), PROJECT).unwrap();
    let connection =
        format!("{CONNECTION}[history]\nschema = \"public\"\ntable = \"_sqlx_migrations\"\n");
    fs::write(directory.join("connection.toml"), connection).unwrap();
    let pin = save_history(directory, &evidence(inventory(directory), Some(180007)));
    fs::rename(&migrations, directory.join("migrations-renamed")).unwrap();

    let committed = run(
        directory,
        &[
            "inspect",
            "--connection",
            "connection.toml",
            "--history",
            "--config",
            "dmg.toml",
            "--base",
            &base,
            "--head",
            &head,
        ],
    );
    assert_exit(&committed, 2);
    assert!(String::from_utf8_lossy(&committed.stderr).contains("inspection CA file"));
    let missing = run(
        directory,
        &[
            "inspect",
            "--connection",
            "connection.toml",
            "--history",
            "--config",
            "dmg.toml",
        ],
    );
    assert_exit(&missing, 3);
    assert!(String::from_utf8_lossy(&missing.stderr).contains("complete candidate inventory"));

    let scan = run(
        directory,
        &[
            "scan",
            "--base",
            &base,
            "--head",
            &head,
            "--format",
            "json",
            "--native-history",
            "history.json",
            "--native-history-sha256",
            &pin.to_string(),
            "--target-id",
            TARGET_ID,
        ],
    );
    assert_exit(&scan, 0);
    let report: Value = serde_json::from_slice(&scan.stdout).unwrap();
    let history = &report["migrations"]["applied_history"]["migrations"];
    assert_eq!(history[0]["status"], "applied");
    assert_eq!(history[1]["status"], "pending");
    assert_eq!(
        report["migrations"]["selected_forwards"],
        json!(["migrations/2_pending.sql"])
    );
}

#[test]
fn rejects_partial_or_conflicting_history_flags() {
    let fixture = tempfile::tempdir().unwrap();
    let dir = fixture.path();
    assert_exit(&run(dir, &["scan", "--native-history", "history.json"]), 2);
    assert_exit(&run(dir, &["scan", "--target-id", TARGET_ID]), 2);
    let pin = ContentDigest::of(b"fixture").to_string();
    let result = run(
        dir,
        &[
            "scan",
            "--native-history",
            "history.json",
            "--native-history-sha256",
            &pin,
            "--target-id",
            TARGET_ID,
            "--applied-manifest",
            "applied.json",
            "--applied-sha256",
            &pin,
        ],
    );
    assert_exit(&result, 2);
    assert!(String::from_utf8_lossy(&result.stderr).contains("ArgumentConflict"));
    assert!(result.stdout.is_empty());
}

#[test]
fn inspection_rejects_unrecognized_connection_fields_and_redacts_errors() {
    let fixture = tempfile::tempdir().unwrap();
    let directory = fixture.path();
    let path = directory.join("connection.toml");
    let output = directory.join("evidence.json");
    fs::write(&output, "original").unwrap();

    for (contents, message) in [
        (
            format!("{CONNECTION}dsn = \"secret-dsn-canary\"\n"),
            "invalid connection configuration TOML",
        ),
        (
            format!("{CONNECTION}[timeouts]\nstatement_mz = 10\n"),
            "invalid connection configuration TOML",
        ),
        (
            format!("{CONNECTION}[timeouts]\noverall_ms = 0\n"),
            "timeouts must be 1 to 30000 milliseconds",
        ),
        (CONNECTION.into(), "inspection CA file"),
    ] {
        fs::write(&path, contents).unwrap();
        let result = run(
            directory,
            &[
                "inspect",
                "--connection",
                "connection.toml",
                "--output",
                "evidence.json",
            ],
        );
        assert_exit(&result, 2);
        let diagnostic = String::from_utf8(result.stderr).unwrap();
        assert!(diagnostic.contains(message));
        for marker in [
            "private-host",
            "private-database",
            "private-user",
            "private-ca",
            "private-password",
            "secret-dsn-canary",
        ] {
            assert!(!diagnostic.contains(marker));
        }
        assert!(result.stdout.is_empty());
        assert_eq!(fs::read_to_string(&output).unwrap(), "original");
    }
}
