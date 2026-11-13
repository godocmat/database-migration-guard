use std::{
    collections::{BTreeMap, BTreeSet},
    ffi::OsString,
    fs,
    path::{Path, PathBuf},
    process::Command,
};

use dmg_core::{
    coverage::{CoverageGapKind, CoverageStatus},
    digest::ContentDigest,
    migration::{ExecutionMode, IntegrityStatus, MigrationDirection, MigrationFile},
    source::{ByteRange, RepoPath, SourceMap},
};
use dmg_discovery::{
    Adapter, Discovery, DiscoveryLimits, DiscoveryOptions, GitRevisions, PlainSql,
    PlainTransaction, SqlxLayout, VersionOrder, discover_git, discover_worktree,
};
use serde::Deserialize;

#[test]
#[ignore = "requires Python 3 and materializes the DMG-02 Git fixtures"]
fn discovery_matches_dmg_02() {
    let fixture_dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/dmg-02");
    let script = fixture_dir.join("corpus.py");
    let python = std::env::var_os("DMG_FIXTURE_PYTHON")
        .unwrap_or_else(|| OsString::from(if cfg!(windows) { "python" } else { "python3" }));
    let output = Command::new(&python)
        .args(["-B"])
        .arg(&script)
        .arg("list")
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let fixtures = String::from_utf8(output.stdout).unwrap();
    let mut count = 0;
    for (index, id) in fixtures.lines().enumerate() {
        // Each PostgreSQL major uses the same discovery inputs and recipes.
        let case_id = id.split('/').next().unwrap();
        if !id.ends_with("/pg17")
            || !matches!(
                case_id,
                "D01"
                    | "D02"
                    | "D03"
                    | "D04"
                    | "D05"
                    | "G01"
                    | "G02"
                    | "G03"
                    | "G04"
                    | "S01"
                    | "T04"
            )
        {
            continue;
        }
        let temp = FixtureTemp::new(index);
        let output = Command::new(&python)
            .arg("-B")
            .arg(&script)
            .args(["materialize", id])
            .arg(&temp.0)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{id}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        let fixture: Fixture =
            serde_json::from_slice(&fs::read(temp.0.join("case.json")).unwrap()).unwrap();
        let repo = temp.0.join("repo");
        let options = fixture.options();
        let worktree = discover_worktree(&repo, &options);
        let revisions = GitRevisions::new(&fixture.git.base, &fixture.git.head).unwrap();
        let git = discover_git(&repo, &revisions, &options);
        fixture.verify_inventory(&worktree, &repo, id);
        fixture.verify_inventory(&git, &repo, id);
        fixture.verify_git(&git, case_id, id);
        let expected_worktree = if matches!(case_id, "D03" | "D05") {
            assert_eq!(
                worktree.plan.coverage.status(),
                CoverageStatus::Incomplete,
                "{id}"
            );
            None
        } else {
            assert_eq!(
                worktree.plan.coverage.status(),
                CoverageStatus::Complete,
                "{id}"
            );
            Some(
                fixture
                    .candidate
                    .iter()
                    .filter(|file| file.direction != "down")
                    .map(|file| file.path.clone())
                    .collect(),
            )
        };
        assert_eq!(worktree.plan.selected_forwards, expected_worktree, "{id}");
        println!("verified {id}");
        count += 1;
    }
    assert_eq!(count, 24);
}

#[derive(Deserialize)]
struct Fixture {
    migration_root: RepoPath,
    git: GitFixture,
    profile: String,
    candidate: Vec<ExpectedFile>,
    files: FixtureFiles,
    discovery: ExpectedDiscovery,
    selected_forward_paths: Option<Vec<RepoPath>>,
    source_spans: Vec<ExpectedSpan>,
}

#[derive(Deserialize)]
struct GitFixture {
    base: String,
    head: String,
    base_available: bool,
    identity_status: Option<BTreeMap<String, Vec<i64>>>,
}

#[derive(Deserialize)]
struct ExpectedFile {
    path: RepoPath,
    version: i64,
    direction: String,
    mode: String,
}

impl ExpectedFile {
    fn direction(&self) -> MigrationDirection {
        if self.direction == "down" {
            MigrationDirection::Down
        } else {
            MigrationDirection::Forward
        }
    }

    fn verify(&self, actual: &MigrationFile, id: &str) {
        assert_eq!(actual.path, self.path, "{id}");
        assert_eq!(
            actual.identity.version.as_numeric(),
            Some(self.version),
            "{id}"
        );
        assert_eq!(actual.direction, self.direction(), "{id}");
        let expected_mode = match self.mode.as_str() {
            "psql-file-transaction" => ExecutionMode::PSQL_FILE_TRANSACTION,
            "psql-autocommit" | "psql-explicit" => ExecutionMode::PSQL_AUTOCOMMIT,
            "sqlx-file-transaction" => ExecutionMode::SQLX_TRANSACTION,
            "sqlx-opt-out-single" | "sqlx-opt-out-batch" => ExecutionMode::SQLX_NO_TRANSACTION,
            other => panic!("unknown fixture mode {other}"),
        };
        assert_eq!(actual.execution, expected_mode, "{id}");
    }
}

#[derive(Deserialize)]
struct FixtureFiles {
    candidate: BTreeMap<RepoPath, ExpectedDigest>,
}

#[derive(Deserialize)]
struct ExpectedDigest {
    sha256: ContentDigest,
}

#[derive(Deserialize)]
struct ExpectedDiscovery {
    duplicate_forward: Vec<RepoPath>,
    unpaired: Vec<RepoPath>,
}

#[derive(Deserialize)]
struct ExpectedSpan {
    path: RepoPath,
    bytes: [usize; 2],
    text: String,
    line: usize,
    column: usize,
}

impl Fixture {
    fn options(&self) -> DiscoveryOptions {
        let adapter = match self.profile.as_str() {
            "plain" => Adapter::Plain(
                PlainSql::new(
                    r"^([0-9]+)_[a-z][a-z0-9_]*\.sql$",
                    VersionOrder::PositiveInteger,
                    PlainTransaction::File,
                )
                .unwrap(),
            ),
            "sqlx-simple" => Adapter::Sqlx(SqlxLayout::Simple),
            "sqlx-reversible" => Adapter::Sqlx(SqlxLayout::Reversible),
            profile => panic!("unknown fixture profile {profile}"),
        };
        DiscoveryOptions::new(
            vec![self.migration_root.clone()],
            adapter,
            DiscoveryLimits::default(),
        )
        .unwrap()
    }

    fn verify_inventory(&self, discovery: &Discovery, repo: &Path, id: &str) {
        assert!(discovery.plan.applied_history.is_none(), "{id}");
        let mut expected = self.candidate.iter().collect::<Vec<_>>();
        expected.sort_by_key(|file| (file.version, file.direction(), &file.path));
        assert_eq!(discovery.plan.inventory.len(), expected.len(), "{id}");
        for (file, expected) in discovery.plan.inventory.iter().zip(expected) {
            expected.verify(file, id);
            assert_eq!(file.digest, self.files.candidate[&file.path].sha256, "{id}");
            let bytes = discovery.source_bytes(&file.path).unwrap();
            assert_eq!(
                bytes,
                fs::read(repo.join(file.path.as_str())).unwrap(),
                "{id}"
            );
            assert_eq!(file.source_segments.len(), 1, "{id}");
            assert_eq!(file.source_segments[0].as_range(), 0..bytes.len(), "{id}");
        }
        for span in &self.source_spans {
            let source = discovery.source(&span.path).unwrap();
            let range = ByteRange::new(span.bytes[0], span.bytes[1]).unwrap();
            assert_eq!(&source[range.as_range()], span.text, "{id}");
            let position = SourceMap::new(span.path.clone(), source)
                .position(range.start())
                .unwrap();
            assert_eq!(
                (position.line.get(), position.column.get()),
                (span.line, span.column),
                "{id}"
            );
        }
    }

    fn verify_git(&self, discovery: &Discovery, case: &str, id: &str) {
        assert_eq!(
            discovery.plan.selected_forwards, self.selected_forward_paths,
            "{id}"
        );
        let gaps = discovery.plan.coverage.gaps();
        match case {
            "D03" | "D05" => {
                let (kind, paths) = if case == "D03" {
                    (
                        CoverageGapKind::AmbiguousIdentity,
                        &self.discovery.duplicate_forward,
                    )
                } else {
                    (CoverageGapKind::UnpairedMigration, &self.discovery.unpaired)
                };
                let actual = gaps
                    .iter()
                    .filter(|gap| gap.kind == kind)
                    .flat_map(|gap| gap.related_paths.iter())
                    .collect::<BTreeSet<_>>();
                assert_eq!(actual, paths.iter().collect(), "{id}");
            }
            "G04" => {
                assert!(!self.git.base_available, "{id}");
                assert!(
                    gaps.iter()
                        .any(|gap| gap.kind == CoverageGapKind::MissingIntegrityEvidence),
                    "{id}"
                );
            }
            _ => assert!(gaps.is_empty(), "{id}: {gaps:?}"),
        }
        let observed = discovery.plan.integrity.as_ref().map(|comparison| {
            let mut statuses = ["unchanged", "new", "modified", "missing"]
                .into_iter()
                .map(|status| (status.to_owned(), BTreeSet::new()))
                .collect::<BTreeMap<_, _>>();
            for row in &comparison.changes {
                let status = match row.status {
                    IntegrityStatus::Unchanged => "unchanged",
                    IntegrityStatus::New => "new",
                    IntegrityStatus::Modified => "modified",
                    IntegrityStatus::Missing => "missing",
                };
                statuses
                    .get_mut(status)
                    .unwrap()
                    .insert(row.identity.version.as_numeric().unwrap());
            }
            statuses
                .into_iter()
                .map(|(status, versions)| (status, versions.into_iter().collect()))
                .collect()
        });
        assert_eq!(observed, self.git.identity_status, "{id}");
    }
}

struct FixtureTemp(PathBuf);

impl FixtureTemp {
    fn new(index: usize) -> Self {
        let unique = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        Self(std::env::temp_dir().join(format!(
            "dmg04-corpus-{}-{index}-{unique}",
            std::process::id()
        )))
    }
}

impl Drop for FixtureTemp {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}
