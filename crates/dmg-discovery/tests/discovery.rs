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
            .map(|file| file.identity.version)
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
