//! Discover migration files and retain the source bytes used by the plan.

mod flyway;
mod git;
mod integrity;
mod manifest;
mod sqlx;
mod worktree;

pub use dmg_core::migration::FlywaySettings;
pub use git::GitRevisions;
pub use manifest::{ManifestError, compare_integrity_manifest, reconcile_applied_manifest};
pub use sqlx::SqlxLayout;

use std::{
    collections::{BTreeMap, BTreeSet},
    path::Path,
};

use dmg_core::{
    coverage::{Coverage, CoverageGap, CoverageGapKind, DiagnosticScope},
    digest::ContentDigest,
    migration::{
        ExecutionMode, IntegrityBasis, MigrationAdapter, MigrationDirection, MigrationFile,
        MigrationIdentity, MigrationPlan,
    },
    source::{ByteRange, RepoPath},
};
use regex::{Regex, RegexBuilder};
use thiserror::Error;

/// Scan explicit top-level migration roots in the current worktree.
pub fn discover_worktree(repository: &Path, options: &DiscoveryOptions) -> Discovery {
    let mut inventory = Inventory::default();
    worktree::collect(repository, options, &mut inventory);
    inventory.finish(options)
}

/// Compare explicit local commits and load the candidate sources from raw Git blobs.
pub fn discover_git(
    repository: &Path,
    revisions: &GitRevisions,
    options: &DiscoveryOptions,
) -> Discovery {
    let baseline = git::collect(repository, &revisions.base, "base", options).finish(options);
    let mut head = git::collect(repository, &revisions.head, "head", options).finish(options);
    if baseline.plan.coverage.gaps().is_empty() && head.plan.coverage.gaps().is_empty() {
        let basis = IntegrityBasis::Git {
            base_commit: revisions.base.clone(),
            head_commit: revisions.head.clone(),
        };
        let comparison = integrity::compare(
            basis,
            integrity::file_digests(&baseline.plan.inventory),
            &head.plan.inventory,
        );
        head.plan.selected_forwards = Some(integrity::select_changed(&comparison));
        head.plan.integrity = Some(comparison);
    } else {
        let gaps = head
            .plan
            .coverage
            .gaps()
            .iter()
            .cloned()
            .chain(
                baseline
                    .plan
                    .coverage
                    .gaps()
                    .iter()
                    .cloned()
                    .map(|mut gap| {
                        gap.message = format!("Git base: {}", gap.message);
                        gap
                    }),
            )
            .collect();
        head.plan.coverage = Coverage::from_gaps(gaps);
        head.plan.selected_forwards = None;
    }
    head
}

/// The reportable plan and its original source inputs.
#[derive(Debug)]
pub struct Discovery {
    pub plan: MigrationPlan,
    sources: BTreeMap<RepoPath, Vec<u8>>,
    head_complete: bool,
    options: DiscoveryOptions,
}

impl Discovery {
    pub fn source_bytes(&self, path: &RepoPath) -> Option<&[u8]> {
        self.sources.get(path).map(Vec::as_slice)
    }

    pub fn source(&self, path: &RepoPath) -> Option<&str> {
        std::str::from_utf8(self.source_bytes(path)?).ok()
    }
}

#[derive(Debug, Clone)]
pub struct DiscoveryOptions {
    roots: Vec<RepoPath>,
    adapter: Adapter,
    limits: DiscoveryLimits,
}

impl DiscoveryOptions {
    pub fn new(
        mut roots: Vec<RepoPath>,
        adapter: Adapter,
        limits: DiscoveryLimits,
    ) -> Result<Self, DiscoveryConfigError> {
        if roots.is_empty() || roots.iter().any(|path| !valid_path(path)) {
            return Err(DiscoveryConfigError::InvalidRoots);
        }
        roots.sort();
        roots.dedup();
        if limits.max_entries == 0
            || limits.max_file_bytes == 0
            || limits.max_total_bytes == 0
            || limits.max_entries > 100_000
            || limits.max_file_bytes > 128 * 1024 * 1024
            || limits.max_total_bytes > 512 * 1024 * 1024
        {
            return Err(DiscoveryConfigError::InvalidLimits);
        }
        Ok(Self {
            roots,
            adapter,
            limits,
        })
    }
}

fn valid_path(path: &RepoPath) -> bool {
    path.as_str().len() <= 1024
        && path.as_str().split('/').all(|part| {
            !part.eq_ignore_ascii_case(".git")
                && !part.ends_with([' ', '.'])
                && !part.contains(['*', '?', '"', '<', '>', '|'])
                && !part.chars().any(char::is_control)
        })
}

#[derive(Debug, Clone, Copy)]
pub struct DiscoveryLimits {
    /// Top-level directory entries across all roots, including ignored entries.
    pub max_entries: usize,
    pub max_file_bytes: usize,
    pub max_total_bytes: usize,
}

impl Default for DiscoveryLimits {
    fn default() -> Self {
        Self {
            max_entries: 20_000,
            max_file_bytes: 8 * 1024 * 1024,
            max_total_bytes: 128 * 1024 * 1024,
        }
    }
}

#[derive(Debug, Error)]
pub enum DiscoveryConfigError {
    #[error("expected at least one portable repository-relative migration root")]
    InvalidRoots,
    #[error("invalid migration entry or byte limits")]
    InvalidLimits,
    #[error("Git base and head must be full commit IDs of the same hash format")]
    InvalidGitRevisions,
    #[error("plain SQL filename pattern must have one capture for the version")]
    InvalidVersionCapture,
    #[error("plain SQL filename pattern must fit within 1024 bytes")]
    PatternTooLarge,
    #[error("invalid plain SQL filename pattern")]
    InvalidPattern(#[source] regex::Error),
}

#[derive(Debug, Clone)]
pub enum Adapter {
    Plain(PlainSql),
    Sqlx(SqlxLayout),
    Flyway(FlywaySettings),
}

#[derive(Debug, Clone)]
pub struct PlainSql {
    filename_pattern: Regex,
    version_order: VersionOrder,
    transaction: PlainTransaction,
}

impl PlainSql {
    /// The expression matches the full filename and has one capture for the version.
    pub fn new(
        filename_pattern: &str,
        version_order: VersionOrder,
        transaction: PlainTransaction,
    ) -> Result<Self, DiscoveryConfigError> {
        if filename_pattern.len() > 1024 {
            return Err(DiscoveryConfigError::PatternTooLarge);
        }
        let pattern = format!(r"\A(?:{filename_pattern})\z");
        let filename_pattern = RegexBuilder::new(&pattern)
            .size_limit(1024 * 1024)
            .dfa_size_limit(1024 * 1024)
            .nest_limit(64)
            .build()
            .map_err(DiscoveryConfigError::InvalidPattern)?;
        if filename_pattern.captures_len() != 2 {
            return Err(DiscoveryConfigError::InvalidVersionCapture);
        }
        Ok(Self {
            filename_pattern,
            version_order,
            transaction,
        })
    }
}

#[derive(Debug, Clone, Copy)]
pub enum VersionOrder {
    PositiveInteger,
}

impl VersionOrder {
    fn version(self, prefix: &str) -> Option<i64> {
        match self {
            Self::PositiveInteger => prefix
                .bytes()
                .all(|byte| byte.is_ascii_digit())
                .then(|| prefix.parse::<i64>().ok().filter(|value| *value > 0))
                .flatten(),
        }
    }
}

/// The outer psql runner behavior. Explicit SQL transaction statements are analyzed later.
#[derive(Debug, Clone, Copy)]
pub enum PlainTransaction {
    File,
    Autocommit,
}

impl PlainTransaction {
    fn execution(self) -> ExecutionMode {
        match self {
            Self::File => ExecutionMode::PSQL_FILE_TRANSACTION,
            Self::Autocommit => ExecutionMode::PSQL_AUTOCOMMIT,
        }
    }
}

struct Candidate {
    identity: MigrationIdentity,
    direction: MigrationDirection,
}

impl Candidate {
    fn supports_manifest_direction(&self, direction: MigrationDirection) -> bool {
        self.direction == direction || self.identity.adapter == MigrationAdapter::Goose
    }
}

struct SourceLayout {
    direction: MigrationDirection,
    source_segments: Vec<ByteRange>,
    execution: ExecutionMode,
}

impl SourceLayout {
    fn whole_file(direction: MigrationDirection, execution: ExecutionMode, bytes: &[u8]) -> Self {
        Self {
            direction,
            source_segments: vec![
                ByteRange::new(0, bytes.len()).expect("file offsets start at zero"),
            ],
            execution,
        }
    }
}

impl Adapter {
    fn kind(&self) -> MigrationAdapter {
        match self {
            Self::Plain(_) => MigrationAdapter::Plain,
            Self::Sqlx(_) => MigrationAdapter::Sqlx,
            Self::Flyway(_) => MigrationAdapter::Flyway,
        }
    }

    fn candidate(&self, filename: &str) -> Result<Option<Candidate>, &'static str> {
        let plain = match self {
            Self::Plain(plain) => plain,
            Self::Sqlx(layout) => return layout.candidate(filename),
            Self::Flyway(_) => return flyway::candidate(filename),
        };
        let Some(captures) = plain.filename_pattern.captures(filename) else {
            return Ok(None);
        };
        let version = captures
            .get(1)
            .and_then(|capture| plain.version_order.version(capture.as_str()))
            .ok_or("matched plain SQL filename has no supported migration version")?;
        Ok(Some(Candidate {
            identity: MigrationIdentity::numeric(MigrationAdapter::Plain, version),
            direction: MigrationDirection::Forward,
        }))
    }

    fn source_layouts(
        &self,
        candidate: &Candidate,
        path: &RepoPath,
        bytes: &[u8],
    ) -> (Vec<SourceLayout>, Option<CoverageGap>) {
        let mut gap = None;
        let execution = match self {
            Self::Plain(plain) => plain.transaction.execution(),
            Self::Sqlx(_) => {
                if std::str::from_utf8(bytes).is_err() {
                    gap = Some(coverage_gap(
                        CoverageGapKind::UnsupportedInput,
                        Some(path),
                        "SQLx requires migration files to contain UTF-8",
                    ));
                }
                sqlx::execution(bytes)
            }
            Self::Flyway(settings) => {
                return flyway::source_layout(*settings, candidate, path, bytes);
            }
        };
        (
            vec![SourceLayout::whole_file(
                candidate.direction,
                execution,
                bytes,
            )],
            gap,
        )
    }
}

#[derive(Default)]
struct Inventory {
    files: Vec<MigrationFile>,
    sources: BTreeMap<RepoPath, Vec<u8>>,
    gaps: Vec<CoverageGap>,
    bytes_read: usize,
}

impl Inventory {
    fn add(&mut self, adapter: &Adapter, candidate: Candidate, path: RepoPath, bytes: Vec<u8>) {
        self.bytes_read += bytes.len();
        let (layouts, gap) = adapter.source_layouts(&candidate, &path, &bytes);
        self.gaps.extend(gap);
        if layouts.is_empty() {
            return;
        }
        let digest = ContentDigest::of(&bytes);
        self.files
            .extend(layouts.into_iter().map(|layout| MigrationFile {
                identity: candidate.identity.clone(),
                direction: layout.direction,
                digest,
                path: path.clone(),
                source_segments: layout.source_segments,
                execution: layout.execution,
            }));
        self.sources.insert(path, bytes);
    }

    fn finish(mut self, options: &DiscoveryOptions) -> Discovery {
        self.files.sort_by(|left, right| {
            (&left.identity, left.direction, &left.path).cmp(&(
                &right.identity,
                right.direction,
                &right.path,
            ))
        });
        for group in self.files.chunk_by(|left, right| {
            left.identity == right.identity && left.direction == right.direction
        }) {
            if group.len() > 1 {
                self.gaps.push(CoverageGap {
                    scope: DiagnosticScope::Migration {
                        identity: group[0].identity.clone(),
                    },
                    kind: CoverageGapKind::AmbiguousIdentity,
                    message: "more than one migration file claims this identity and direction"
                        .into(),
                    related_paths: group.iter().map(|file| file.path.clone()).collect(),
                });
            }
        }
        sqlx::validate_pairs(&options.adapter, &self.files, &mut self.gaps);
        let selected_forwards = self.gaps.is_empty().then(|| {
            self.files
                .iter()
                .filter(|file| file.direction == MigrationDirection::Forward)
                .map(|file| file.path.clone())
                .collect()
        });
        Discovery {
            head_complete: self.gaps.is_empty(),
            options: options.clone(),
            plan: MigrationPlan {
                inventory: self.files,
                selected_forwards,
                integrity: None,
                applied_history: None,
                coverage: Coverage::from_gaps(self.gaps),
            },
            sources: self.sources,
        }
    }
}

fn coverage_gap(kind: CoverageGapKind, path: Option<&RepoPath>, message: &str) -> CoverageGap {
    CoverageGap {
        scope: path.map_or(DiagnosticScope::Repository, |path| DiagnosticScope::Path {
            path: path.clone(),
        }),
        kind,
        message: message.into(),
        related_paths: Vec::new(),
    }
}

fn record_candidate(
    inventory: &mut Inventory,
    options: &DiscoveryOptions,
    path: &RepoPath,
    filename: &str,
) -> Option<Candidate> {
    match options.adapter.candidate(filename) {
        Ok(candidate) => candidate,
        Err(message) => {
            inventory.gaps.push(coverage_gap(
                CoverageGapKind::UnsupportedInput,
                Some(path),
                message,
            ));
            None
        }
    }
}

fn budget_for_file(inventory: &Inventory, options: &DiscoveryOptions) -> usize {
    options
        .limits
        .max_file_bytes
        .min(options.limits.max_total_bytes - inventory.bytes_read)
}

fn sorted_entries<T: Ord>(entries: impl Iterator<Item = T>, limit: usize) -> Option<BTreeSet<T>> {
    let mut bounded = BTreeSet::new();
    for (index, entry) in entries.enumerate() {
        if index == limit {
            return None;
        }
        bounded.insert(entry);
    }
    Some(bounded)
}
