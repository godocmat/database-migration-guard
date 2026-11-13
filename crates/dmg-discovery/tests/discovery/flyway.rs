use dmg_core::{
    coverage::DiagnosticScope,
    migration::{
        IntegrityStatus, MigrationDirection, MigrationIdentity, StatementDelivery,
        TransactionWrapper,
    },
};
use dmg_discovery::{FlywaySettings, GitRevisions, discover_git};

use super::*;

fn options(roots: &[&str]) -> DiscoveryOptions {
    DiscoveryOptions::new(
        roots
            .iter()
            .map(|root| RepoPath::new(*root).unwrap())
            .collect(),
        Adapter::Flyway(FlywaySettings::default()),
        DiscoveryLimits::default(),
    )
    .unwrap()
}

#[test]
fn orders_versioned_repeatable_and_undo_inputs_and_retains_original_bytes() {
    let repo = TestRepo::new().unwrap();
    let sql = "\u{feff}-- café 🧪\r\nSELECT 1;\r\n".as_bytes();
    for name in [
        "V1.10__middle.sql",
        "V2.sql",
        "V10__last.SQL",
        "U01_2__undo.sql",
        "R__alpha0.sql",
        "R__alpha_z.sql",
        "R__\u{10000}.sql",
        "R__\u{e000}.sql",
        "R__.sql",
    ] {
        repo.write(format!("migrations/{name}"), b"SELECT 2;\n");
    }
    repo.write("migrations/V01.0__base.sql", sql);
    repo.write("migrations/V1_2.00__early.sql", b"SELECT 2;\n");
    repo.write("migrations/readme.md", b"metadata");

    let discovery = discover_worktree(&repo.0, &options(&["migrations"]));
    let plan = discovery.plan.clone();
    assert_eq!(plan.coverage.status(), CoverageStatus::Complete);
    let names = plan
        .inventory
        .iter()
        .map(|file| file.path.as_str().strip_prefix("migrations/").unwrap())
        .collect::<Vec<_>>();
    assert_eq!(
        names,
        [
            "V01.0__base.sql",
            "V1_2.00__early.sql",
            "U01_2__undo.sql",
            "V1.10__middle.sql",
            "V2.sql",
            "V10__last.SQL",
            "R__.sql",
            "R__alpha_z.sql",
            "R__alpha0.sql",
            "R__\u{10000}.sql",
            "R__\u{e000}.sql",
        ]
    );
    assert_eq!(plan.inventory[2].direction, MigrationDirection::Down);
    assert_eq!(plan.inventory[2].identity, plan.inventory[1].identity);
    assert_eq!(
        plan.inventory[2].identity,
        MigrationIdentity::flyway_version("1.2").unwrap()
    );
    let selected = plan.selected_forwards.unwrap();
    assert_eq!(selected.len(), names.len() - 1);
    assert!(!selected.contains(&plan.inventory[2].path));
    let base = &plan.inventory[0];
    assert_eq!(base.source_segments[0].as_range(), 3..sql.len());
    assert_eq!(base.digest, ContentDigest::of(sql));
    assert_eq!(discovery.source_bytes(&base.path), Some(sql));
    assert_eq!(
        base.execution.transaction_wrapper,
        TransactionWrapper::Flyway(FlywaySettings::default())
    );
    assert_eq!(
        base.execution.statement_delivery,
        StatementDelivery::PerStatement
    );
}

#[test]
fn duplicate_aliases_fail_closed() {
    let repo = TestRepo::new().unwrap();
    for name in [
        "V01.2__one.sql",
        "V1_02.0__two.sql",
        "R__a b.sql",
        "R__a_b.sql",
    ] {
        repo.write(format!("migrations/{name}"), b"SELECT 1;\n");
    }
    let plan = discover_worktree(&repo.0, &options(&["migrations"])).plan;
    assert!(plan.selected_forwards.is_none());
    assert_eq!(plan.coverage.gaps().len(), 2);
    assert!(plan.coverage.gaps().iter().all(|gap| {
        gap.kind == CoverageGapKind::AmbiguousIdentity && gap.related_paths.len() == 2
    }));
}

#[test]
fn recognizes_unsupported_inputs_and_requires_recursive_locations() {
    let repo = TestRepo::new().unwrap();
    for name in [
        "beforeMigrate__first.SQL",
        "afterMigrate__script",
        "createSchema.sh",
        "V1__script.py",
        "B1__baseline.sql",
        "V9__custom.psql",
        "V3__configure.SQL.CoNf",
        "Vbad.sql",
    ] {
        repo.write(format!("migrations/{name}"), b"SELECT 1;\n");
    }
    repo.write("migrations/nested/V4__nested.sql", b"SELECT 1;\n");
    repo.write("migrations/U4__undo.sql", b"SELECT '${schema}';");
    let plan = discover_worktree(&repo.0, &options(&["migrations"])).plan;
    assert!(plan.selected_forwards.is_none());
    assert_eq!(plan.coverage.gaps().len(), 10);
    assert!(plan.coverage.gaps().iter().all(|gap| {
        gap.kind == CoverageGapKind::UnsupportedInput
            && matches!(gap.scope, DiagnosticScope::Path { .. })
    }));
    assert!(
        plan.coverage
            .gaps()
            .iter()
            .any(|gap| gap.message.contains("sidecar"))
    );
    assert!(
        plan.coverage
            .gaps()
            .iter()
            .any(|gap| gap.message.contains("nested"))
    );
    assert!(
        plan.coverage
            .gaps()
            .iter()
            .any(|gap| gap.message.contains("placeholder"))
    );
}

#[test]
fn compares_git_repeatable_revisions_and_bound_nested_locations() {
    let repo = TestRepo::new().unwrap();
    init_git(&repo.0);
    let versioned = "migrations/V1__base.sql";
    let repeatable = "migrations/nested/R__summary.sql";
    let undo = "migrations/nested/U1__undo.sql";
    repo.write(versioned, b"SELECT 1;\n");
    repo.write(repeatable, b"SELECT 2;\n");
    repo.write(undo, b"SELECT 3;\n");
    let base = commit(&repo.0, "base");
    let revised = b"SELECT 4;\r\n";
    repo.write(repeatable, revised);
    let head = commit(&repo.0, "repeatable revision");
    let revisions = GitRevisions::new(&base, &head).unwrap();
    let paths = options(&["migrations", "migrations/nested"]);
    let discovery = discover_git(&repo.0, &revisions, &paths);
    let selected = discovery.plan.selected_forwards.as_ref().unwrap();
    assert_eq!(selected, &[RepoPath::new(repeatable).unwrap()]);
    assert_eq!(
        discovery.source_bytes(&selected[0]),
        Some(revised.as_slice())
    );
    assert_eq!(
        discover_worktree(&repo.0, &paths).plan.inventory,
        discovery.plan.inventory
    );
    let changes = &discovery.plan.integrity.as_ref().unwrap().changes;
    assert_eq!(changes.len(), 3);
    assert_eq!(changes[0].status, IntegrityStatus::Unchanged);
    assert_eq!(changes[1].status, IntegrityStatus::Unchanged);
    assert!(changes[2].identity.is_repeatable());
    assert_eq!(changes[2].status, IntegrityStatus::Modified);

    let unconfigured = discover_git(&repo.0, &revisions, &options(&["migrations"])).plan;
    assert!(unconfigured.selected_forwards.is_none());
    assert!(unconfigured.integrity.is_none());
    assert_eq!(unconfigured.coverage.gaps().len(), 2);
    assert!(unconfigured.coverage.gaps().iter().all(|gap| matches!(
        &gap.scope, DiagnosticScope::Path { path } if path.as_str() == "migrations/nested"
    )));

    repo.write(
        "migrations/nested/R__summary.sql.conf",
        b"executeInTransaction=false\n",
    );
    let head_with_sidecar = commit(&repo.0, "sidecar added without changing SQL");
    let sidecar_revisions = GitRevisions::new(&head, &head_with_sidecar).unwrap();
    let result = discover_git(&repo.0, &sidecar_revisions, &paths);
    assert!(result.plan.selected_forwards.is_none());
    assert!(result.plan.integrity.is_none());
    let [gap] = result.plan.coverage.gaps() else {
        panic!("unexpected diagnostics: {:?}", result.plan.coverage);
    };
    assert!(matches!(&gap.scope, DiagnosticScope::Path { path }
        if path.as_str() == "migrations/nested/R__summary.sql.conf"));
}

#[cfg(unix)]
#[test]
fn linked_locations_fail_closed_even_when_the_link_is_not_named_like_sql() {
    let repo = TestRepo::new().unwrap();
    init_git(&repo.0);
    repo.write("outside/V1__source.sql", b"SELECT 1;\n");
    fs::create_dir(repo.0.join("migrations")).unwrap();
    std::os::unix::fs::symlink("../outside", repo.0.join("migrations/linked")).unwrap();
    let head = commit(&repo.0, "linked migration location");
    let revisions = GitRevisions::new(&head, &head).unwrap();
    let options = options(&["migrations"]);
    for plan in [
        discover_worktree(&repo.0, &options).plan,
        discover_git(&repo.0, &revisions, &options).plan,
    ] {
        assert!(plan.inventory.is_empty());
        assert!(plan.selected_forwards.is_none());
        assert!(plan.coverage.gaps().iter().any(|gap| {
            gap.kind == CoverageGapKind::InvalidPath
                && matches!(&gap.scope, DiagnosticScope::Path { path }
                    if path.as_str() == "migrations/linked")
        }));
    }
}
