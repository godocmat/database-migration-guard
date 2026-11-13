use std::{fs, path::Path, process::Command};

use dmg_core::{
    digest::ContentDigest,
    inspection::{InspectionScope, TargetId},
    migration::StatementDelivery,
    rehearsal::RehearsalMode,
    source::RepoPath,
    target::PostgresTarget,
};
use dmg_discovery::{
    Adapter, Discovery, DiscoveryLimits, DiscoveryOptions, GitRevisions, PlainSql,
    PlainTransaction, VersionOrder, discover_git, discover_worktree,
};

use crate::{
    PostgresImage, RehearsalContext, RehearsalInput, SeedKind, expected_rehearsal_plan,
    prepare_rehearsal,
};

fn git(path: &Path, args: &[&str]) -> String {
    let output = Command::new("git")
        .current_dir(path)
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
    assert!(output.status.success(), "git {args:?} failed");
    std::str::from_utf8(&output.stdout)
        .unwrap()
        .trim()
        .to_owned()
}

fn discover(adapter: Adapter, base: &[(&str, &[u8])], head: &[(&str, &[u8])]) -> Discovery {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path();
    git(path, &["init", "--quiet"]);
    fs::create_dir(path.join("migrations")).unwrap();
    for (name, sql) in base {
        fs::write(path.join("migrations").join(name), sql).unwrap();
    }
    git(path, &["add", "migrations"]);
    git(path, &["commit", "--quiet", "--allow-empty", "-m", "base"]);
    let base = git(path, &["rev-parse", "HEAD"]);
    for (name, sql) in head {
        fs::write(path.join("migrations").join(name), sql).unwrap();
    }
    git(path, &["add", "migrations"]);
    git(
        path,
        &["commit", "--quiet", "--allow-empty", "-m", "candidate"],
    );
    let head = git(path, &["rev-parse", "HEAD"]);
    discover_git(
        path,
        &GitRevisions::new(&base, &head).unwrap(),
        &options(adapter),
    )
}

fn options(adapter: Adapter) -> DiscoveryOptions {
    DiscoveryOptions::new(
        vec![RepoPath::new("migrations").unwrap()],
        adapter,
        DiscoveryLimits::default(),
    )
    .unwrap()
}

fn plain(transaction: PlainTransaction) -> Adapter {
    Adapter::Plain(
        PlainSql::new(
            r"(\d+)_[a-z]+\.sql",
            VersionOrder::PositiveInteger,
            transaction,
        )
        .unwrap(),
    )
}

fn context(target: PostgresTarget) -> RehearsalContext {
    RehearsalContext {
        repository_id: TargetId::new("test-repository").unwrap(),
        target,
        target_id: TargetId::new("test-target").unwrap(),
        scope: InspectionScope::new(vec!["public".into()]).unwrap(),
        search_path: vec!["public".into()],
        project_config_digest: ContentDigest::of(b"configuration"),
        policy_digest: ContentDigest::of(b"policy"),
    }
}

#[test]
fn git_plan_keeps_pending_order_and_exact_original_spans() {
    let baseline = b"CREATE TABLE items (id integer PRIMARY KEY);";
    let candidate = "/* café */\r\nALTER TABLE items ADD COLUMN note text;\r\nSELECT 7;\r\n";
    let discovery = discover(
        plain(PlainTransaction::Autocommit),
        &[("1_create.sql", baseline)],
        &[
            ("10_select.sql", b"SELECT 10;"),
            ("2_change.sql", candidate.as_bytes()),
        ],
    );
    let plan = prepare_rehearsal(
        &discovery,
        context(PostgresTarget::V18),
        RehearsalInput::SchemaOnly {
            baseline_sql: baseline,
        },
        PostgresImage::V18_7,
    )
    .unwrap();
    let (digest, migrations) = expected_rehearsal_plan(
        &discovery,
        plan.binding().target,
        &plan.binding().scope,
        &plan.binding().mode,
    )
    .unwrap();
    assert_eq!(digest, plan.binding().execution_plan_digest);
    assert_eq!(migrations, plan.expected_migrations());
    assert_eq!(
        migrations
            .iter()
            .map(|m| m.path.as_str())
            .collect::<Vec<_>>(),
        ["migrations/2_change.sql", "migrations/10_select.sql"]
    );
    assert_eq!(
        migrations[0].digest,
        ContentDigest::of(candidate.as_bytes())
    );
    let first = &migrations[0].groups[0];
    assert_eq!(first.delivery, StatementDelivery::PerStatement);
    assert_eq!(
        &candidate[first.statements[0].span.bytes.as_range()],
        "ALTER TABLE items ADD COLUMN note text"
    );
}

#[test]
fn refuses_ambiguous_applied_base_and_unsupported_setup() {
    let baseline = b"CREATE TABLE items (id integer PRIMARY KEY);";
    let discovery = discover(
        plain(PlainTransaction::Autocommit),
        &[("2_create.sql", baseline)],
        &[("1_earlier.sql", b"SELECT 1;")],
    );
    assert!(
        prepare_rehearsal(
            &discovery,
            context(PostgresTarget::V18),
            RehearsalInput::SchemaOnly {
                baseline_sql: baseline
            },
            PostgresImage::V18_7
        )
        .err()
        .unwrap()
        .coverage()
        .is_some()
    );

    let discovery = discover(
        plain(PlainTransaction::Autocommit),
        &[],
        &[("1_create.sql", baseline)],
    );
    assert!(
        prepare_rehearsal(
            &discovery,
            context(PostgresTarget::V18),
            RehearsalInput::Seeded {
                baseline_sql: baseline,
                seed_sql: b"DELETE FROM items;",
                seed_kind: SeedKind::Synthetic
            },
            PostgresImage::V18_7
        )
        .err()
        .unwrap()
        .coverage()
        .is_some()
    );
}

#[test]
fn refuses_unbound_worktree_and_unknown_runner_commands() {
    let dir = tempfile::tempdir().unwrap();
    fs::create_dir(dir.path().join("migrations")).unwrap();
    let discovery = discover_worktree(dir.path(), &options(plain(PlainTransaction::File)));
    let scope = context(PostgresTarget::V18).scope;
    assert!(
        expected_rehearsal_plan(
            &discovery,
            PostgresTarget::V18,
            &scope,
            &RehearsalMode::EmptyDatabase
        )
        .is_err()
    );
    let discovery = discover(
        plain(PlainTransaction::File),
        &[],
        &[("1_session.sql", b"SET ROLE postgres;")],
    );
    assert!(
        expected_rehearsal_plan(
            &discovery,
            PostgresTarget::V18,
            &scope,
            &RehearsalMode::EmptyDatabase
        )
        .is_err()
    );
}

#[test]
fn worker_scope_rejects_authored_schema_creation_and_external_relations() {
    let discovery = discover(
        plain(PlainTransaction::Autocommit),
        &[],
        &[("1_schema.sql", b"CREATE SCHEMA IF NOT EXISTS public;")],
    );
    let scope = context(PostgresTarget::V18).scope;
    let error = expected_rehearsal_plan(
        &discovery,
        PostgresTarget::V18,
        &scope,
        &RehearsalMode::EmptyDatabase,
    )
    .unwrap_err();
    assert_eq!(
        error.coverage().unwrap().gaps()[0].kind,
        dmg_core::coverage::CoverageGapKind::UnsupportedSql
    );

    let discovery = discover(
        plain(PlainTransaction::Autocommit),
        &[],
        &[(
            "1_outside.sql",
            b"ALTER TABLE elsewhere.items ADD COLUMN note text;",
        )],
    );
    let error = expected_rehearsal_plan(
        &discovery,
        PostgresTarget::V18,
        &scope,
        &RehearsalMode::EmptyDatabase,
    )
    .unwrap_err();
    assert_eq!(
        error.coverage().unwrap().gaps()[0].kind,
        dmg_core::coverage::CoverageGapKind::UnresolvedDependency
    );
}

#[cfg(all(target_os = "linux", target_arch = "x86_64"))]
mod matrix;

mod protocol_fixture;
