use std::{
    fs, io,
    path::{Path, PathBuf},
    sync::atomic::{AtomicUsize, Ordering},
};

use dmg_core::{
    coverage::{CoverageGapKind, CoverageStatus},
    digest::ContentDigest,
    migration::ExecutionMode,
    source::RepoPath,
};
use dmg_discovery::{
    Adapter, DiscoveryLimits, DiscoveryOptions, PlainSql, PlainTransaction, VersionOrder,
    discover_worktree,
};

static SEQUENCE: AtomicUsize = AtomicUsize::new(0);

struct TestRepo(PathBuf);

impl TestRepo {
    fn new() -> io::Result<Self> {
        loop {
            let sequence = SEQUENCE.fetch_add(1, Ordering::Relaxed);
            let root =
                std::env::temp_dir().join(format!("dmg04-{}-{sequence}", std::process::id()));
            match fs::create_dir(&root) {
                Ok(()) => return Ok(Self(root)),
                Err(error) if error.kind() == io::ErrorKind::AlreadyExists => continue,
                Err(error) => return Err(error),
            }
        }
    }

    fn write(&self, path: impl AsRef<Path>, bytes: &[u8]) {
        let file = self.0.join(path);
        fs::create_dir_all(file.parent().unwrap()).unwrap();
        fs::write(file, bytes).unwrap();
    }
}

impl Drop for TestRepo {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn plain(limits: DiscoveryLimits) -> DiscoveryOptions {
    DiscoveryOptions::new(
        vec![RepoPath::new("migrations").unwrap()],
        Adapter::Plain(
            PlainSql::new(
                r"^([0-9]+)_[a-z][a-z0-9_]*\.sql$",
                VersionOrder::PositiveInteger,
                PlainTransaction::File,
            )
            .unwrap(),
        ),
        limits,
    )
    .unwrap()
}

#[test]
fn plain_inventory_preserves_bytes_and_numeric_order() {
    let repo = TestRepo::new().unwrap();
    let second = "-- café 🧪\r\nSELECT 2;\r\n".as_bytes();
    repo.write("migrations/10_tenth.sql", b"SELECT 10;\n");
    repo.write("migrations/2_second.sql", second);
    repo.write("migrations/001_first.sql", b"SELECT 1;\n");
    repo.write("migrations/README.sql", b"ignored\n");
    repo.write("migrations/archive/3_archive.sql", b"ignored\n");

    let discovery = discover_worktree(&repo.0, &plain(DiscoveryLimits::default()));
    assert_eq!(discovery.plan.coverage.status(), CoverageStatus::Complete);
    let files = &discovery.plan.inventory;
    assert_eq!(
        files
            .iter()
            .map(|file| file.identity.version.as_numeric().unwrap())
            .collect::<Vec<_>>(),
        [1, 2, 10]
    );
    let selected = discovery.plan.selected_forwards.as_ref().unwrap();
    assert_eq!(
        selected,
        &files
            .iter()
            .map(|file| file.path.clone())
            .collect::<Vec<_>>()
    );
    assert_eq!(discovery.source_bytes(&files[1].path), Some(second));
    assert_eq!(files[1].digest, ContentDigest::of(second));
    assert_eq!(files[1].source_segments[0].as_range(), 0..second.len());
    assert_eq!(files[1].execution, ExecutionMode::PSQL_FILE_TRANSACTION);
}

#[test]
fn duplicate_versions_make_selection_incomplete() {
    let repo = TestRepo::new().unwrap();
    repo.write("migrations/2_first.sql", b"SELECT 2;");
    repo.write("migrations/02_second.sql", b"SELECT 2;");
    let discovery = discover_worktree(&repo.0, &plain(DiscoveryLimits::default()));
    assert_eq!(discovery.plan.inventory.len(), 2);
    assert!(discovery.plan.selected_forwards.is_none());
    let gaps = discovery.plan.coverage.gaps();
    assert_eq!(gaps.len(), 1);
    assert_eq!(gaps[0].kind, CoverageGapKind::AmbiguousIdentity);
    assert_eq!(gaps[0].related_paths.len(), 2);
}

#[test]
fn file_and_directory_limits_produce_scoped_gaps() {
    let repo = TestRepo::new().unwrap();
    repo.write("migrations/1_first.sql", b"SELECT 1;");
    repo.write("migrations/2_second.sql", b"SELECT 2;");

    let limits = DiscoveryLimits {
        max_entries: 10,
        max_file_bytes: 8,
        max_total_bytes: 16,
    };
    let discovery = discover_worktree(&repo.0, &plain(limits));
    assert_eq!(discovery.plan.inventory.len(), 0);
    assert_eq!(discovery.plan.coverage.gaps().len(), 2);

    let discovery = discover_worktree(
        &repo.0,
        &plain(DiscoveryLimits {
            max_entries: 1,
            ..limits
        }),
    );
    assert!(discovery.plan.selected_forwards.is_none());
    let gaps = discovery.plan.coverage.gaps();
    assert_eq!(gaps.len(), 1);
    assert_eq!(gaps[0].kind, CoverageGapKind::InputLimitExceeded);
}

#[cfg(unix)]
#[test]
fn symlink_migration_is_a_gap() {
    let repo = TestRepo::new().unwrap();
    repo.write("other/1_source.sql", b"SELECT 1;");
    fs::create_dir(repo.0.join("migrations")).unwrap();
    std::os::unix::fs::symlink(
        "../other/1_source.sql",
        repo.0.join("migrations/1_first.sql"),
    )
    .unwrap();
    let discovery = discover_worktree(&repo.0, &plain(DiscoveryLimits::default()));
    assert!(discovery.plan.inventory.is_empty());
    assert_eq!(
        discovery.plan.coverage.gaps()[0].kind,
        CoverageGapKind::InvalidPath
    );
}

#[test]
fn sqlx_simple_uses_the_native_byte_zero_transaction_directive() {
    let repo = TestRepo::new().unwrap();
    let one = b"-- no-transaction\r\nSELECT 1;\r\nSELECT 2;\r\n";
    let two = b"\n-- no-transaction\nSELECT 3;\n";
    repo.write("migrations/01_a.sql", one);
    repo.write("migrations/2_b.sql", two);
    let discovery = discover_worktree(&repo.0, &sqlx(dmg_discovery::SqlxLayout::Simple));
    assert_eq!(discovery.plan.coverage.status(), CoverageStatus::Complete);
    let files = &discovery.plan.inventory;
    assert_eq!(files[0].execution, ExecutionMode::SQLX_NO_TRANSACTION);
    assert_eq!(files[1].execution, ExecutionMode::SQLX_TRANSACTION);
    assert_eq!(files[0].source_segments[0].as_range(), 0..one.len());
    assert_eq!(discovery.source_bytes(&files[0].path), Some(one.as_slice()));
}

#[test]
fn sqlx_reversible_requires_a_pair_and_selects_only_up() {
    use dmg_core::migration::MigrationDirection;
    let repo = TestRepo::new().unwrap();
    let options = sqlx(dmg_discovery::SqlxLayout::Reversible);
    repo.write("migrations/002_a.up.sql", b"SELECT 1;");
    repo.write("migrations/2_a.down.sql", b"SELECT 2;");
    let discovery = discover_worktree(&repo.0, &options);
    assert_eq!(discovery.plan.coverage.status(), CoverageStatus::Complete);
    assert_eq!(discovery.plan.inventory.len(), 2);
    assert_eq!(
        discovery.plan.inventory[1].direction,
        MigrationDirection::Down
    );
    assert_eq!(
        discovery.plan.selected_forwards.unwrap(),
        [discovery.plan.inventory[0].path.clone()]
    );

    fs::rename(
        repo.0.join("migrations/2_a.down.sql"),
        repo.0.join("migrations/2_b.down.sql"),
    )
    .unwrap();
    let discovery = discover_worktree(&repo.0, &options);
    assert!(discovery.plan.selected_forwards.is_none());
    let gaps = discovery.plan.coverage.gaps();
    assert_eq!(gaps.len(), 1);
    assert_eq!(gaps[0].kind, CoverageGapKind::UnpairedMigration);
    assert_eq!(gaps[0].related_paths.len(), 2);
}

fn sqlx(layout: dmg_discovery::SqlxLayout) -> DiscoveryOptions {
    DiscoveryOptions::new(
        vec![RepoPath::new("migrations").unwrap()],
        Adapter::Sqlx(layout),
        DiscoveryLimits::default(),
    )
    .unwrap()
}

fn goose() -> DiscoveryOptions {
    DiscoveryOptions::new(
        vec![RepoPath::new("migrations").unwrap()],
        Adapter::Goose,
        DiscoveryLimits::default(),
    )
    .unwrap()
}

#[test]
fn goose_tracks_shared_directional_ranges_and_native_order() {
    use dmg_core::migration::{
        MigrationAdapter, MigrationDirection, StatementDelivery, TransactionWrapper,
    };

    let repo = TestRepo::new().unwrap();
    let forward = "SELECT 'é;🧪';";
    let grouped = "SELECT 2;\r\nSELECT 3";
    let down = "SELECT 4;";
    let sql = format!(
        "  \r\n-- heading\r\n-- +goose UP\r\n\r\n  {forward}\r\n-- +goose StatementBegin\r\n{grouped}\r\n-- +goose StatementEnd\r\n  \r\n-- +goose Down\r\n-- +goose no transaction\r\n{down}\r\n",
    );
    repo.write("migrations/10_sections.sql", sql.as_bytes());
    repo.write("migrations/+3_.sql", b"-- +goose Up\n");
    repo.write("migrations/02_base.sql", b"-- +goose Up\nSELECT 1;\n");
    repo.write("migrations/4_ignored.SQL", b"not a migration");
    repo.write("migrations/archive/1_ignored.sql", b"not a migration");
    repo.write("migrations/5_test.go", b"not a migration");
    repo.write("migrations/helpers.go", b"not a migration");

    let discovery = discover_worktree(&repo.0, &goose());
    assert_eq!(discovery.plan.coverage.status(), CoverageStatus::Complete);
    let inventory = &discovery.plan.inventory;
    let selected = discovery.plan.selected_forwards.as_ref().unwrap();
    assert_eq!(
        selected.iter().map(RepoPath::as_str).collect::<Vec<_>>(),
        [
            "migrations/02_base.sql",
            "migrations/+3_.sql",
            "migrations/10_sections.sql",
        ]
    );
    assert_eq!(
        inventory
            .iter()
            .map(|file| (
                file.identity.adapter,
                file.identity.version.as_numeric().unwrap(),
                file.direction
            ))
            .collect::<Vec<_>>(),
        [
            (MigrationAdapter::Goose, 2, MigrationDirection::Forward),
            (MigrationAdapter::Goose, 3, MigrationDirection::Forward),
            (MigrationAdapter::Goose, 10, MigrationDirection::Forward),
            (MigrationAdapter::Goose, 10, MigrationDirection::Down),
        ]
    );
    assert!(inventory[1].source_segments[0].is_empty());
    assert_eq!(
        inventory[0].execution.transaction_wrapper,
        TransactionWrapper::File
    );
    for (file, queries) in [
        (&inventory[2], vec![forward, grouped]),
        (&inventory[3], vec![down]),
    ] {
        assert_eq!(file.digest, ContentDigest::of(sql.as_bytes()));
        assert_eq!(discovery.source(&file.path), Some(sql.as_str()));
        assert_eq!(file.execution.transaction_wrapper, TransactionWrapper::None);
        assert_eq!(
            file.execution.statement_delivery,
            StatementDelivery::SingleQuery
        );
        assert_eq!(
            file.source_segments
                .iter()
                .map(|span| &sql[span.as_range()])
                .collect::<Vec<_>>(),
            queries,
        );
    }
}

#[test]
fn goose_compares_both_directions_when_only_down_changes_in_git() {
    use dmg_core::migration::{IntegrityStatus, MigrationDirection};

    let repo = TestRepo::new().unwrap();
    init_git(&repo.0);
    let path = "migrations/1_both.sql";
    let base_sql = b"-- +goose Up\r\nSELECT 1;\r\n-- +goose Down\r\nSELECT 2;\r\n";
    let head_sql = b"-- +goose Up\r\nSELECT 1;\r\n-- +goose Down\r\nSELECT 3;\r\n";
    repo.write(path, base_sql);
    let base = commit(&repo.0, "base");
    repo.write(path, head_sql);
    let head = commit(&repo.0, "head");
    repo.write(path, b"untracked working bytes");

    let revisions = dmg_discovery::GitRevisions::new(&base, &head).unwrap();
    let discovery = dmg_discovery::discover_git(&repo.0, &revisions, &goose());
    let selected = discovery.plan.selected_forwards.as_ref().unwrap();
    assert_eq!(selected, &[RepoPath::new(path).unwrap()]);
    assert_eq!(
        discovery.source_bytes(&selected[0]),
        Some(head_sql.as_slice())
    );
    let changes = &discovery.plan.integrity.as_ref().unwrap().changes;
    assert_eq!(changes.len(), 2);
    assert_eq!(changes[0].direction, MigrationDirection::Forward);
    assert_eq!(changes[1].direction, MigrationDirection::Down);
    for change in changes {
        assert_eq!(change.status, IntegrityStatus::Modified);
        assert_eq!(
            change.before.as_ref().unwrap().digest,
            ContentDigest::of(base_sql)
        );
        assert_eq!(
            change.after.as_ref().unwrap().digest,
            ContentDigest::of(head_sql)
        );
    }
}

#[test]
fn goose_rejects_unsupported_scripts_and_ambiguous_versions() {
    use dmg_core::coverage::DiagnosticScope;
    use dmg_discovery::DiscoveryConfigError;

    let repo = TestRepo::new().unwrap();
    let options = goose();
    for (name, source) in [
        (
            "1_envsub.sql",
            "-- +goose Up\n-- +goose ENVSUB ON\nSELECT 1;\n",
        ),
        (
            "1_unknown.sql",
            "-- +goose Up\n-- +goose SomethingElse\nSELECT 1;\n",
        ),
        ("1_indented.sql", " -- +goose Up\nSELECT 1;\n"),
        ("1_bom.sql", "\u{feff}-- +goose Up\nSELECT 1;\n"),
        (
            "1_unclosed.sql",
            "-- +goose Up\n-- +goose StatementBegin\nSELECT 1;\n",
        ),
        (
            "1_empty_group.sql",
            "-- +goose Up\n-- +goose StatementBegin\n-- +goose StatementEnd\nSELECT 1;\n",
        ),
        (
            "1_unfinished.sql",
            "-- +goose Up\nSELECT 1\n-- +goose Down\n",
        ),
        (
            "1_noncontiguous.sql",
            "-- +goose Up\nSELECT\n-- +goose NO TRANSACTION\n1;\n",
        ),
    ] {
        let path = format!("migrations/{name}");
        repo.write(&path, source.as_bytes());
        let discovery = discover_worktree(&repo.0, &options);
        assert!(discovery.plan.selected_forwards.is_none(), "{name}");
        let gaps = discovery.plan.coverage.gaps();
        assert_eq!(gaps.len(), 1, "{name}");
        assert_eq!(gaps[0].kind, CoverageGapKind::UnsupportedInput, "{name}");
        assert!(
            matches!(&gaps[0].scope, DiagnosticScope::Span { span } if span.path.as_str() == path)
        );
        fs::remove_file(repo.0.join(path)).unwrap();
    }
    let valid = b"-- +goose Up\nSELECT 1;\n";
    for path in [
        "migrations/02_alias.sql",
        "migrations/+2_alias.sql",
        "migrations/0_zero.sql",
        "migrations/3_unregistered.go",
        "migrations/4_dir.sql/nested.txt",
    ] {
        repo.write(path, valid);
    }
    let discovery = discover_worktree(&repo.0, &options);
    assert!(discovery.plan.selected_forwards.is_none());
    let gaps = discovery.plan.coverage.gaps();
    assert_eq!(
        gaps.iter()
            .filter(|gap| gap.kind == CoverageGapKind::AmbiguousIdentity)
            .count(),
        1
    );
    assert_eq!(
        gaps.iter()
            .filter(|gap| gap.kind == CoverageGapKind::UnsupportedInput)
            .count(),
        2
    );
    assert_eq!(
        gaps.iter()
            .filter(|gap| gap.kind == CoverageGapKind::InvalidPath)
            .count(),
        1
    );

    let multi_root = DiscoveryOptions::new(
        vec![RepoPath::new("a").unwrap(), RepoPath::new("b").unwrap()],
        Adapter::Goose,
        DiscoveryLimits::default(),
    );
    assert!(matches!(
        multi_root,
        Err(DiscoveryConfigError::SingleRootRequired)
    ));
}

#[test]
fn git_compares_raw_bytes_and_keeps_unchanged_head_sources() {
    use dmg_core::migration::IntegrityStatus::{Missing, Modified, New, Unchanged};
    let repo = TestRepo::new().unwrap();
    init_git(&repo.0);
    let first = b"-- base\nSELECT 1;\n";
    repo.write("migrations/01_first.sql", first);
    repo.write("migrations/02_second.sql", b"SELECT 2;\n");
    repo.write("migrations/03_third.sql", b"SELECT 3;\n");
    repo.write("migrations/04_fourth.sql", b"SELECT 4;\n");
    let base = commit(&repo.0, "base");

    let changed = b"-- base\r\nSELECT 1;\r\n";
    repo.write("migrations/01_first.sql", changed);
    fs::rename(
        repo.0.join("migrations/02_second.sql"),
        repo.0.join("migrations/2_renamed.sql"),
    )
    .unwrap();
    fs::rename(
        repo.0.join("migrations/03_third.sql"),
        repo.0.join("migrations/5_moved.sql"),
    )
    .unwrap();
    let head = commit(&repo.0, "candidate");
    repo.write("migrations/01_first.sql", b"SELECT 999;");
    repo.write("migrations/9_untracked.sql", b"SELECT 999;");
    git(&repo.0, &["replace", &head, &base]);

    let revisions = dmg_discovery::GitRevisions::new(&base, &head).unwrap();
    let discovery =
        dmg_discovery::discover_git(&repo.0, &revisions, &plain(DiscoveryLimits::default()));
    assert_eq!(discovery.plan.coverage.status(), CoverageStatus::Complete);
    let comparison = discovery.plan.integrity.as_ref().unwrap();
    assert_eq!(
        comparison
            .changes
            .iter()
            .map(|row| row.status)
            .collect::<Vec<_>>(),
        [Modified, Unchanged, Missing, Unchanged, New]
    );
    let moved = &comparison.changes[1];
    assert_ne!(
        moved.before.as_ref().unwrap().path,
        moved.after.as_ref().unwrap().path
    );
    assert_eq!(
        discovery.source_bytes(&RepoPath::new("migrations/01_first.sql").unwrap()),
        Some(changed.as_slice())
    );
    assert_eq!(
        discovery.source(&RepoPath::new("migrations/04_fourth.sql").unwrap()),
        Some("SELECT 4;\n")
    );
    assert_eq!(
        discovery
            .plan
            .selected_forwards
            .as_ref()
            .unwrap()
            .iter()
            .map(RepoPath::as_str)
            .collect::<Vec<_>>(),
        [
            "migrations/01_first.sql",
            "migrations/2_renamed.sql",
            "migrations/5_moved.sql"
        ]
    );
    assert!(discovery.plan.applied_history.is_none());
}

#[test]
fn unavailable_or_noncommit_base_preserves_explicit_head_inventory() {
    let repo = TestRepo::new().unwrap();
    init_git(&repo.0);
    let sql = b"SELECT 1;\n";
    repo.write("migrations/1_first.sql", sql);
    let head = commit(&repo.0, "candidate");
    let blob = git(&repo.0, &["rev-parse", "HEAD:migrations/1_first.sql"]);
    for base in ["0".repeat(40), blob] {
        let revisions = dmg_discovery::GitRevisions::new(&base, &head).unwrap();
        let discovery =
            dmg_discovery::discover_git(&repo.0, &revisions, &plain(DiscoveryLimits::default()));
        assert!(discovery.plan.selected_forwards.is_none());
        assert!(discovery.plan.integrity.is_none());
        assert_eq!(
            discovery.plan.coverage.gaps()[0].kind,
            CoverageGapKind::MissingIntegrityEvidence
        );
        let file = &discovery.plan.inventory[0];
        assert_eq!(discovery.source_bytes(&file.path), Some(sql.as_slice()));
    }
}

fn git(repo: &Path, args: &[&str]) -> String {
    let mut command = std::process::Command::new("git");
    for (key, _) in std::env::vars_os() {
        if key
            .to_string_lossy()
            .to_ascii_uppercase()
            .starts_with("GIT_")
        {
            command.env_remove(key);
        }
    }
    let output = command
        .current_dir(repo)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env(
            "GIT_CONFIG_GLOBAL",
            if cfg!(windows) { "NUL" } else { "/dev/null" },
        )
        .args(args)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "git {args:?} failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).unwrap().trim().into()
}

fn init_git(repo: &Path) {
    git(repo, &["init", "--quiet", "--object-format=sha1"]);
    git(repo, &["config", "user.name", "DMG Fixture"]);
    git(repo, &["config", "user.email", "fixture@example.invalid"]);
    git(repo, &["config", "core.autocrlf", "true"]);
    fs::write(repo.join(".gitattributes"), "*.sql -text\n").unwrap();
}

fn commit(repo: &Path, message: &str) -> String {
    git(repo, &["add", "--all"]);
    git(repo, &["commit", "--quiet", "--no-gpg-sign", "-m", message]);
    git(repo, &["rev-parse", "HEAD"])
}

#[path = "discovery/flyway.rs"]
mod flyway;

#[test]
fn integrity_and_applied_manifests_classify_independent_evidence() {
    use dmg_core::migration::{AppliedStatus, IntegrityStatus};
    let repo = TestRepo::new().unwrap();
    let one = b"SELECT 1;";
    let two = b"SELECT 2;";
    let changed_two = b"SELECT 20;";
    let three = b"SELECT 3;";
    let four = b"SELECT 4;";
    let five = b"SELECT 5;";
    repo.write("migrations/1_a.sql", one);
    repo.write("migrations/2_b.sql", changed_two);
    repo.write("migrations/4_d.sql", four);
    repo.write("migrations/5_e.sql", five);

    let mut discovery = discover_worktree(&repo.0, &plain(DiscoveryLimits::default()));
    assert!(discovery.plan.applied_history.is_none());
    let baseline = serde_json::to_vec(&serde_json::json!({
        "schema_version": 1,
        "adapter": "plain",
        "migrations": [
            {"version": 1, "direction": "forward", "path": "migrations/1_a.sql", "sha256": ContentDigest::of(one)},
            {"version": 2, "direction": "forward", "path": "migrations/2_b.sql", "sha256": ContentDigest::of(two)},
            {"version": 3, "direction": "forward", "path": "migrations/3_c.sql", "sha256": ContentDigest::of(three)},
        ],
    })).unwrap();
    let applied = serde_json::to_vec(&serde_json::json!({
        "schema_version": 1,
        "adapter": "plain",
        "migrations": [
            {"version": 1, "status": "applied", "sha256": ContentDigest::of(one)},
            {"version": 2, "status": "applied", "sha256": ContentDigest::of(two)},
            {"version": 3, "status": "applied", "sha256": ContentDigest::of(three)},
            {"version": 5, "status": "failed", "sha256": ContentDigest::of(five)},
        ],
    }))
    .unwrap();

    let baseline_digest = ContentDigest::of(&baseline);
    let applied_digest = ContentDigest::of(&applied);
    assert!(matches!(
        dmg_discovery::compare_integrity_manifest(&mut discovery, &baseline, applied_digest),
        Err(dmg_discovery::ManifestError::DigestMismatch)
    ));
    assert!(discovery.plan.integrity.is_none());
    dmg_discovery::compare_integrity_manifest(&mut discovery, &baseline, baseline_digest).unwrap();
    assert!(discovery.plan.applied_history.is_none());
    dmg_discovery::reconcile_applied_manifest(&mut discovery, &applied, applied_digest).unwrap();

    let integrity = discovery.plan.integrity.as_ref().unwrap();
    assert_eq!(
        integrity
            .changes
            .iter()
            .map(|row| row.status)
            .collect::<Vec<_>>(),
        [
            IntegrityStatus::Unchanged,
            IntegrityStatus::Modified,
            IntegrityStatus::Missing,
            IntegrityStatus::New,
            IntegrityStatus::New,
        ]
    );
    let history = discovery.plan.applied_history.as_ref().unwrap();
    assert_eq!(
        history
            .migrations
            .iter()
            .map(|row| row.status)
            .collect::<Vec<_>>(),
        [
            AppliedStatus::Applied,
            AppliedStatus::Modified,
            AppliedStatus::Missing,
            AppliedStatus::Pending,
            AppliedStatus::Failed,
        ]
    );
}

#[test]
fn applied_history_can_be_verified_when_only_the_git_base_is_missing() {
    let repo = TestRepo::new().unwrap();
    init_git(&repo.0);
    repo.write("migrations/1_first.sql", b"SELECT 1;");
    let head = commit(&repo.0, "candidate");
    let revisions = dmg_discovery::GitRevisions::new(&"0".repeat(40), &head).unwrap();
    let mut discovery =
        dmg_discovery::discover_git(&repo.0, &revisions, &plain(DiscoveryLimits::default()));
    let applied = br#"{"schema_version":1,"adapter":"plain","migrations":[]}"#;
    dmg_discovery::reconcile_applied_manifest(&mut discovery, applied, ContentDigest::of(applied))
        .unwrap();
    assert_eq!(
        discovery.plan.applied_history.unwrap().migrations[0].status,
        dmg_core::migration::AppliedStatus::Pending
    );
    assert_eq!(discovery.plan.coverage.status(), CoverageStatus::Incomplete);
    assert!(discovery.plan.integrity.is_none());
}

#[test]
fn reversible_integrity_manifest_requires_matching_directions() {
    let repo = TestRepo::new().unwrap();
    let content = b"SELECT 1;";
    repo.write("migrations/1_a.up.sql", content);
    repo.write("migrations/1_a.down.sql", content);
    let mut discovery = discover_worktree(&repo.0, &sqlx(dmg_discovery::SqlxLayout::Reversible));
    let mut baseline = serde_json::json!({
        "schema_version": 1,
        "adapter": "sqlx",
        "migrations": [
            {"version": 1, "direction": "forward", "path": "migrations/1_a.up.sql", "sha256": ContentDigest::of(content)},
        ],
    });
    let bytes = serde_json::to_vec(&baseline).unwrap();
    assert!(matches!(
        dmg_discovery::compare_integrity_manifest(
            &mut discovery,
            &bytes,
            ContentDigest::of(&bytes)
        ),
        Err(dmg_discovery::ManifestError::UnpairedMigration)
    ));
    baseline["migrations"].as_array_mut().unwrap().push(serde_json::json!({
        "version": 1, "direction": "down", "path": "migrations/1_a.down.sql", "sha256": ContentDigest::of(content),
    }));
    let bytes = serde_json::to_vec(&baseline).unwrap();
    dmg_discovery::compare_integrity_manifest(&mut discovery, &bytes, ContentDigest::of(&bytes))
        .unwrap();
    assert_eq!(discovery.plan.integrity.unwrap().changes.len(), 2);
    assert_eq!(discovery.plan.selected_forwards, Some(Vec::new()));
}
