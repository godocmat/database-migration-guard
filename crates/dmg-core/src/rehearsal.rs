//! Revision-bound evidence from a disposable PostgreSQL rehearsal.

use serde::{Deserialize, Serialize};

use crate::{
    coverage::CoverageStatus,
    diff::SchemaDiffV1,
    digest::ContentDigest,
    inspection::{InspectionScope, TargetId},
    migration::{ExecutionMode, MigrationAdapter, MigrationIdentity, StatementDelivery},
    schema::SchemaVersionV1,
    snapshot::{ConstraintKind, QualifiedName},
    source::{RepoPath, SourceSpan},
    target::PostgresTarget,
};

/// Version of the supported runner replay and worker environment semantics.
pub const REHEARSAL_PROFILE_VERSION: u32 = 1;

#[derive(Debug, Clone, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RehearsalBindingV1 {
    pub repository_id: TargetId,
    pub base_commit: String,
    pub head_commit: String,
    pub project_config_digest: ContentDigest,
    pub policy_digest: ContentDigest,
    pub forward_inventory_digest: ContentDigest,
    pub execution_plan_digest: ContentDigest,
    pub adapter: MigrationAdapter,
    pub profile_version: u32,
    pub target: PostgresTarget,
    pub target_id: TargetId,
    pub server_version_num: u32,
    pub image_manifest_digest: ContentDigest,
    pub image_config_digest: ContentDigest,
    pub scope: InspectionScope,
    pub search_path: Vec<String>,
    pub mode: RehearsalMode,
}

impl RehearsalBindingV1 {
    pub fn digest(&self) -> Result<ContentDigest, serde_json::Error> {
        serde_json::to_vec(self).map(|bytes| ContentDigest::of(&bytes))
    }

    fn is_valid(&self) -> bool {
        self.profile_version == REHEARSAL_PROFILE_VERSION
            && matches!(self.base_commit.len(), 40 | 64)
            && self.base_commit.len() == self.head_commit.len()
            && [&self.base_commit, &self.head_commit].iter().all(|commit| {
                commit
                    .bytes()
                    .all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
            })
            && self.server_version_num / 10_000 == u32::from(self.target.major())
            && valid_rehearsal_search_path(&self.scope, &self.search_path)
            && self.mode.scripts().all(|script| script.bytes != 0)
    }
}

/// Validate the exact schema resolution order within a provisioned rehearsal scope.
pub fn valid_rehearsal_search_path(scope: &InspectionScope, search_path: &[String]) -> bool {
    !search_path.is_empty()
        && search_path.len() <= scope.schemas().len()
        && search_path.iter().enumerate().all(|(index, schema)| {
            scope.schemas().binary_search(schema).is_ok()
                && !search_path[..index].contains(schema)
                && schema != "$user"
        })
}

#[derive(Debug, Clone, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum RehearsalMode {
    EmptyDatabase,
    SchemaOnly {
        baseline: RehearsalScriptV1,
    },
    Seeded {
        baseline: RehearsalScriptV1,
        seed: RehearsalScriptV1,
        seed_kind: SeedKind,
    },
}

impl RehearsalMode {
    fn scripts(&self) -> impl Iterator<Item = RehearsalScriptV1> {
        let scripts = match *self {
            Self::EmptyDatabase => [None, None],
            Self::SchemaOnly { baseline } => [Some(baseline), None],
            Self::Seeded { baseline, seed, .. } => [Some(baseline), Some(seed)],
        };
        scripts.into_iter().flatten()
    }
}

#[derive(Debug, Clone, Copy, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RehearsalScriptV1 {
    pub digest: ContentDigest,
    pub bytes: u64,
}

#[derive(Debug, Clone, Copy, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SeedKind {
    Synthetic,
    Sanitized,
}

#[derive(Debug, Clone, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RehearsalReportV1 {
    schema_version: SchemaVersionV1,
    pub binding: RehearsalBindingV1,
    pub started_at_unix_seconds: u64,
    pub completed_at_unix_seconds: Option<u64>,
    pub elapsed_millis: u64,
    pub outcome: RehearsalOutcome,
    pub cleanup: RehearsalCleanup,
    /// Generated Docker worker name for operator reconciliation after unconfirmed cleanup.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cleanup_worker: Option<String>,
    pub failure: Option<RehearsalFailureV1>,
    pub baseline_elapsed_millis: Option<u64>,
    pub seed_elapsed_millis: Option<u64>,
    pub seeded_rows_inserted: Option<u64>,
    pub before: Option<RehearsalSchemaV1>,
    pub after: Option<RehearsalSchemaV1>,
    pub schema_diff: Option<SchemaDiffV1>,
    pub migrations: Vec<RehearsalMigrationV1>,
}

impl RehearsalReportV1 {
    pub fn new(binding: RehearsalBindingV1, started_at_unix_seconds: u64) -> Self {
        Self {
            schema_version: SchemaVersionV1,
            binding,
            started_at_unix_seconds,
            completed_at_unix_seconds: None,
            elapsed_millis: 0,
            outcome: RehearsalOutcome::Incomplete,
            cleanup: RehearsalCleanup::NotObserved,
            cleanup_worker: None,
            failure: None,
            baseline_elapsed_millis: None,
            seed_elapsed_millis: None,
            seeded_rows_inserted: None,
            before: None,
            after: None,
            schema_diff: None,
            migrations: Vec::new(),
        }
    }

    /// Check structural invariants before using an already authenticated report.
    pub fn validate(&self) -> Result<(), InvalidRehearsalEvidence> {
        let observed = |snapshot: &RehearsalSchemaV1| {
            snapshot.server_version_num == self.binding.server_version_num
                && snapshot.observed_at_unix_seconds >= self.started_at_unix_seconds
                && self
                    .completed_at_unix_seconds
                    .is_none_or(|end| snapshot.observed_at_unix_seconds <= end)
        };
        if !self.binding.is_valid()
            || self
                .completed_at_unix_seconds
                .is_some_and(|end| end < self.started_at_unix_seconds)
            || !self.before.as_ref().is_none_or(observed)
            || !self.after.as_ref().is_none_or(observed)
            || !self
                .failure
                .as_ref()
                .is_none_or(RehearsalFailureV1::is_valid)
            || !self.cleanup_worker.as_deref().is_none_or(|worker| {
                self.cleanup == RehearsalCleanup::Unconfirmed
                    && worker.strip_prefix("dmg-worker-").is_some_and(|suffix| {
                        suffix.len() == 32
                            && suffix
                                .bytes()
                                .all(|byte| matches!(byte, b'0'..=b'9' | b'a'..=b'f'))
                    })
            })
        {
            return Err(InvalidRehearsalEvidence);
        }
        if self
            .before
            .as_ref()
            .zip(self.after.as_ref())
            .is_some_and(|(before, after)| {
                before.observed_at_unix_seconds > after.observed_at_unix_seconds
            })
        {
            return Err(InvalidRehearsalEvidence);
        }
        if let Some(diff) = &self.schema_diff {
            let matches_observation =
                |source: &crate::diff::DiffSource, summary: Option<&RehearsalSchemaV1>| {
                    summary.is_some_and(|summary| {
                        source.structural_digest == Some(summary.structural_digest)
                            && source.observation.as_ref().is_some_and(|observation| {
                                observation.target_id == self.binding.target_id
                                    && observation.server_version_num
                                        == Some(summary.server_version_num)
                                    && observation.observed_at_unix_seconds
                                        == summary.observed_at_unix_seconds
                            })
                    })
                };
            if !diff.same_requested_scope
                || !diff
                    .schemas
                    .iter()
                    .map(|schema| &schema.schema)
                    .eq(self.binding.scope.schemas())
                || !matches_observation(&diff.before, self.before.as_ref())
                || !matches_observation(&diff.after, self.after.as_ref())
            {
                return Err(InvalidRehearsalEvidence);
            }
        }
        match self.outcome {
            RehearsalOutcome::Succeeded
                if self.cleanup == RehearsalCleanup::Verified
                    && self.completed_at_unix_seconds.is_some()
                    && self.failure.is_none()
                    && self.schema_diff.is_some()
                    && self
                        .migrations
                        .iter()
                        .all(|migration| migration.state == RehearsalMigrationState::Completed) =>
            {
                Ok(())
            }
            RehearsalOutcome::Failed
                if self.cleanup == RehearsalCleanup::Verified
                    && self.completed_at_unix_seconds.is_some()
                    && self.failure.as_ref().is_some_and(|failure| {
                        failure.phase == RehearsalPhase::Migration
                            && failure.reason == RehearsalFailureReason::Database
                            && failure.sqlstate.is_some()
                    }) =>
            {
                Ok(())
            }
            RehearsalOutcome::Incomplete | RehearsalOutcome::TimedOut => Ok(()),
            _ => Err(InvalidRehearsalEvidence),
        }
    }
}

#[derive(Debug, thiserror::Error)]
#[error("invalid rehearsal evidence")]
pub struct InvalidRehearsalEvidence;

#[derive(Debug, Clone, Copy, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RehearsalOutcome {
    Succeeded,
    Failed,
    Incomplete,
    TimedOut,
}

#[derive(Debug, Clone, Copy, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RehearsalCleanup {
    NotObserved,
    Verified,
    Unconfirmed,
}

#[derive(Debug, Clone, Copy, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RehearsalPhase {
    Provision,
    Baseline,
    Seed,
    Before,
    Migration,
    After,
    Cleanup,
    Clock,
}

#[derive(Debug, Clone, Copy, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RehearsalFailureReason {
    UnsupportedPlatform,
    RuntimeUnavailable,
    Worker,
    Transport,
    ProtocolLimit,
    Database,
    Deadline,
    OpenTransaction,
    SnapshotUnavailable,
    CleanupUnconfirmed,
    Clock,
}

#[derive(Debug, Clone, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RehearsalFailureV1 {
    pub phase: RehearsalPhase,
    pub reason: RehearsalFailureReason,
    pub sqlstate: Option<String>,
    pub path: Option<RepoPath>,
    pub span: Option<SourceSpan>,
}

impl RehearsalFailureV1 {
    fn is_valid(&self) -> bool {
        self.sqlstate.as_ref().is_none_or(|state| {
            state.len() == 5
                && state
                    .bytes()
                    .all(|b| matches!(b, b'0'..=b'9' | b'A'..=b'Z'))
        }) && self
            .span
            .as_ref()
            .is_none_or(|span| self.path.as_ref() == Some(&span.path))
    }
}

#[derive(Debug, Clone, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RehearsalSchemaV1 {
    pub structural_digest: ContentDigest,
    pub server_version_num: u32,
    pub observed_at_unix_seconds: u64,
    pub coverage: CoverageStatus,
    pub constraints: Vec<RehearsalConstraintV1>,
}

#[derive(Debug, Clone, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RehearsalConstraintV1 {
    pub table: QualifiedName,
    pub name: String,
    pub kind: ConstraintKind,
    pub validated: bool,
    pub enforced: bool,
}

#[derive(Debug, Clone, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RehearsalMigrationV1 {
    pub identity: MigrationIdentity,
    pub path: RepoPath,
    pub digest: ContentDigest,
    pub execution: ExecutionMode,
    pub state: RehearsalMigrationState,
    pub elapsed_millis: Option<u64>,
    pub groups: Vec<RehearsalGroupV1>,
}

#[derive(Debug, Clone, Copy, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RehearsalMigrationState {
    Skipped,
    Completed,
    Failed,
    Interrupted,
}

#[derive(Debug, Clone, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RehearsalGroupV1 {
    pub delivery: StatementDelivery,
    pub elapsed_millis: Option<u64>,
    pub statements: Vec<RehearsalStatementV1>,
}

#[derive(Debug, Clone, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RehearsalStatementV1 {
    pub span: SourceSpan,
    pub state: RehearsalStatementState,
    pub durability: Option<RehearsalDurability>,
    /// Client-observed command completion time since the group was sent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub elapsed_millis: Option<u64>,
}

#[derive(Debug, Clone, Copy, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RehearsalStatementState {
    Skipped,
    Completed,
    Failed,
    Unknown,
}

#[derive(Debug, Clone, Copy, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RehearsalDurability {
    Committed,
    RolledBack,
    Unknown,
}
