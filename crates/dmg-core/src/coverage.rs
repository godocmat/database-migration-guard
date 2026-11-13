//! Coverage diagnostics scoped to the original inputs.

use serde::{Deserialize, Serialize};

use crate::{
    migration::MigrationIdentity,
    source::{RepoPath, SourceSpan},
};

#[derive(Debug, Default, Clone, Copy, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CoverageStatus {
    #[default]
    Complete,
    Incomplete,
}

#[derive(Debug, Default, Clone, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum Coverage {
    #[default]
    Complete,
    Incomplete {
        gaps: Vec<CoverageGap>,
    },
}

impl Coverage {
    pub fn from_gaps(mut gaps: Vec<CoverageGap>) -> Self {
        if gaps.is_empty() {
            return Self::Complete;
        }

        gaps.sort();
        gaps.dedup();
        Self::Incomplete { gaps }
    }

    pub fn status(&self) -> CoverageStatus {
        match self {
            Self::Complete => CoverageStatus::Complete,
            Self::Incomplete { .. } => CoverageStatus::Incomplete,
        }
    }

    pub fn gaps(&self) -> &[CoverageGap] {
        match self {
            Self::Complete => &[],
            Self::Incomplete { gaps } => gaps,
        }
    }
}

#[derive(Debug, Clone, Eq, PartialEq, Ord, PartialOrd, Serialize, Deserialize)]
pub struct CoverageGap {
    pub scope: DiagnosticScope,
    pub kind: CoverageGapKind,
    pub message: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub related_paths: Vec<RepoPath>,
}

#[derive(Debug, Clone, Eq, PartialEq, Ord, PartialOrd, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum DiagnosticScope {
    Repository,
    Migration { identity: MigrationIdentity },
    Path { path: RepoPath },
    Span { span: SourceSpan },
}

#[derive(Debug, Clone, Copy, Eq, PartialEq, Ord, PartialOrd, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CoverageGapKind {
    InvalidPath,
    UnreadableInput,
    UnsupportedInput,
    InputLimitExceeded,
    AmbiguousIdentity,
    UnpairedMigration,
    MissingIntegrityEvidence,
    MissingAppliedHistory,
    ParseError,
    UnsupportedSql,
    IncompatibleTarget,
    UnresolvedDependency,
    UnresolvedTransactionMode,
}
