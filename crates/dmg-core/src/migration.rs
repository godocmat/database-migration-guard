//! Adapter inventory, execution settings, and evidence from migration discovery.

mod version;

pub use version::{
    FlywayRepeatable, FlywayVersion, InvalidMigrationVersion, MigrationVersion, NativeVersion,
    SupabaseVersion,
};

use serde::{Deserialize, Serialize};

use crate::{
    coverage::Coverage,
    digest::ContentDigest,
    history::NativeHistoryMethod,
    source::{ByteRange, RepoPath},
};

#[derive(Debug, Clone, Copy, Eq, PartialEq, Ord, PartialOrd, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MigrationAdapter {
    Plain,
    Sqlx,
    Flyway,
    Goose,
    Supabase,
}

impl MigrationAdapter {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Plain => "plain",
            Self::Sqlx => "sqlx",
            Self::Flyway => "flyway",
            Self::Goose => "goose",
            Self::Supabase => "supabase",
        }
    }
}

/// An adapter-native identity shared by the forward and reverse directions.
#[derive(Debug, Clone, Eq, PartialEq, Ord, PartialOrd, Hash, Serialize, Deserialize)]
#[serde(try_from = "IdentityFields")]
pub struct MigrationIdentity {
    pub adapter: MigrationAdapter,
    pub version: MigrationVersion,
}

impl MigrationIdentity {
    pub fn numeric(adapter: MigrationAdapter, version: i64) -> Self {
        Self {
            adapter,
            version: MigrationVersion::Numeric(version),
        }
    }

    pub fn flyway_version(version: &str) -> Result<Self, InvalidMigrationVersion> {
        Ok(Self {
            adapter: MigrationAdapter::Flyway,
            version: MigrationVersion::Native(NativeVersion::Flyway(FlywayVersion::new(version)?)),
        })
    }

    pub fn flyway_repeatable(description: &str) -> Result<Self, InvalidMigrationVersion> {
        Ok(Self {
            adapter: MigrationAdapter::Flyway,
            version: MigrationVersion::Native(NativeVersion::Repeatable(FlywayRepeatable::new(
                description,
            )?)),
        })
    }

    pub fn supabase_version(version: &str) -> Result<Self, InvalidMigrationVersion> {
        Ok(Self {
            adapter: MigrationAdapter::Supabase,
            version: MigrationVersion::Native(NativeVersion::Supabase(SupabaseVersion::new(
                version,
            )?)),
        })
    }

    pub fn from_version(
        adapter: MigrationAdapter,
        version: MigrationVersion,
    ) -> Result<Self, InvalidMigrationVersion> {
        let valid = matches!(
            (adapter, &version),
            (
                MigrationAdapter::Plain | MigrationAdapter::Sqlx | MigrationAdapter::Goose,
                MigrationVersion::Numeric(_)
            ) | (
                MigrationAdapter::Flyway,
                MigrationVersion::Native(NativeVersion::Flyway(_) | NativeVersion::Repeatable(_))
            ) | (
                MigrationAdapter::Supabase,
                MigrationVersion::Native(NativeVersion::Supabase(_))
            )
        );
        if !valid {
            return Err(InvalidMigrationVersion);
        }
        Ok(Self { adapter, version })
    }

    pub fn is_repeatable(&self) -> bool {
        matches!(
            self.version,
            MigrationVersion::Native(NativeVersion::Repeatable(_))
        )
    }
}

#[derive(Deserialize)]
struct IdentityFields {
    adapter: MigrationAdapter,
    version: MigrationVersion,
}

impl TryFrom<IdentityFields> for MigrationIdentity {
    type Error = InvalidMigrationVersion;

    fn try_from(fields: IdentityFields) -> Result<Self, Self::Error> {
        Self::from_version(fields.adapter, fields.version)
    }
}

#[derive(Debug, Clone, Copy, Eq, PartialEq, Ord, PartialOrd, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MigrationDirection {
    Forward,
    Down,
}

#[derive(Debug, Clone, Eq, PartialEq, Serialize, Deserialize)]
pub struct MigrationFile {
    pub identity: MigrationIdentity,
    pub path: RepoPath,
    pub direction: MigrationDirection,
    pub digest: ContentDigest,
    /// Original-file byte ranges in runner order. A shared file keeps its digest in both directions.
    /// For single-query delivery, each segment is one simple Query. For extended batches,
    /// each segment is one batch and a standalone boundary requires a separate execution mode.
    pub source_segments: Vec<ByteRange>,
    pub execution: ExecutionMode,
}

#[derive(Debug, Clone, Copy, Eq, PartialEq, Serialize, Deserialize)]
pub struct ExecutionMode {
    pub transaction_wrapper: TransactionWrapper,
    pub statement_delivery: StatementDelivery,
}

impl ExecutionMode {
    pub const PSQL_FILE_TRANSACTION: Self = Self {
        transaction_wrapper: TransactionWrapper::File,
        statement_delivery: StatementDelivery::PerStatement,
    };
    pub const PSQL_AUTOCOMMIT: Self = Self {
        transaction_wrapper: TransactionWrapper::None,
        statement_delivery: StatementDelivery::PerStatement,
    };
    pub const SQLX_TRANSACTION: Self = Self {
        transaction_wrapper: TransactionWrapper::File,
        statement_delivery: StatementDelivery::SingleQuery,
    };
    pub const SQLX_NO_TRANSACTION: Self = Self {
        transaction_wrapper: TransactionWrapper::None,
        statement_delivery: StatementDelivery::SingleQuery,
    };
    pub const SUPABASE_BATCH: Self = Self {
        transaction_wrapper: TransactionWrapper::None,
        statement_delivery: StatementDelivery::ExtendedBatch,
    };
}

#[derive(Debug, Clone, Copy, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TransactionWrapper {
    None,
    File,
    /// Flyway decides its effective wrapper after classifying the file's statements.
    Flyway(FlywaySettings),
}

/// The supported Flyway PostgreSQL transaction settings declared by the operator.
#[derive(Debug, Clone, Copy, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FlywaySettings {
    pub execute_in_transaction: bool,
    pub mixed: bool,
    pub transactional_lock: bool,
}

impl Default for FlywaySettings {
    fn default() -> Self {
        Self {
            execute_in_transaction: true,
            mixed: false,
            transactional_lock: true,
        }
    }
}

#[derive(Debug, Clone, Copy, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StatementDelivery {
    PerStatement,
    SingleQuery,
    ExtendedBatch,
}

#[derive(Debug, Clone, Eq, PartialEq, Serialize, Deserialize)]
pub struct MigrationPlan {
    /// Head inventory in adapter order, including Down files.
    pub inventory: Vec<MigrationFile>,
    /// Ordered forward paths to analyze, or `None` when selection could not be resolved.
    pub selected_forwards: Option<Vec<RepoPath>>,
    pub integrity: Option<IntegrityComparison>,
    pub applied_history: Option<AppliedHistory>,
    pub coverage: Coverage,
}

/// The baseline used for original-file integrity comparisons.
#[derive(Debug, Clone, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum IntegrityBasis {
    Git {
        base_commit: String,
        head_commit: String,
    },
    Manifest {
        digest: ContentDigest,
    },
}

#[derive(Debug, Clone, Eq, PartialEq, Serialize, Deserialize)]
pub struct IntegrityComparison {
    pub basis: IntegrityBasis,
    pub changes: Vec<IntegrityChange>,
}

#[derive(Debug, Clone, Eq, PartialEq, Serialize, Deserialize)]
pub struct IntegrityChange {
    pub identity: MigrationIdentity,
    pub direction: MigrationDirection,
    pub status: IntegrityStatus,
    pub before: Option<FileDigest>,
    pub after: Option<FileDigest>,
}

#[derive(Debug, Clone, Eq, PartialEq, Serialize, Deserialize)]
pub struct FileDigest {
    pub path: RepoPath,
    pub digest: ContentDigest,
}

#[derive(Debug, Clone, Copy, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum IntegrityStatus {
    Unchanged,
    New,
    Modified,
    Missing,
}

/// A classification derived from authorized applied-history evidence.
#[derive(Debug, Clone, Eq, PartialEq, Serialize, Deserialize)]
pub struct AppliedHistory {
    pub basis: AppliedHistoryBasis,
    pub migrations: Vec<AppliedMigration>,
}

#[derive(Debug, Clone, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum AppliedHistoryBasis {
    Manifest {
        digest: ContentDigest,
    },
    Native {
        digest: ContentDigest,
        inventory_digest: ContentDigest,
        adapter: MigrationAdapter,
        method: NativeHistoryMethod,
    },
}

#[derive(Debug, Clone, Eq, PartialEq, Serialize, Deserialize)]
pub struct AppliedMigration {
    pub identity: MigrationIdentity,
    pub status: AppliedStatus,
}

#[derive(Debug, Default, Clone, Copy, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AppliedStatus {
    Pending,
    Applied,
    Modified,
    Missing,
    Failed,
    #[default]
    Unknown,
}
