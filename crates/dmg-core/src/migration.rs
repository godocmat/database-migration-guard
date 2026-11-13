//! Adapter inventory, execution settings, and evidence from migration discovery.

use serde::{Deserialize, Serialize};

use crate::{
    coverage::Coverage,
    digest::ContentDigest,
    source::{ByteRange, RepoPath},
};

#[derive(Debug, Clone, Copy, Eq, PartialEq, Ord, PartialOrd, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MigrationAdapter {
    Plain,
    Sqlx,
}

/// MVP migration identity. Direction is tracked separately for reversible pairs.
#[derive(Debug, Clone, Eq, PartialEq, Ord, PartialOrd, Hash, Serialize, Deserialize)]
pub struct MigrationIdentity {
    pub adapter: MigrationAdapter,
    pub version: i64,
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
    /// Original-file byte ranges in runner order.
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
}

#[derive(Debug, Clone, Copy, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TransactionWrapper {
    None,
    File,
}

#[derive(Debug, Clone, Copy, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StatementDelivery {
    PerStatement,
    SingleQuery,
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
    Manifest { digest: ContentDigest },
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
