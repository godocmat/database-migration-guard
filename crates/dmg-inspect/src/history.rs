//! Bounded native migration history observations for pinned runner profiles.

use std::{
    collections::{BTreeMap, BTreeSet},
    time::{SystemTime, UNIX_EPOCH},
};

use dmg_core::{
    digest::ContentDigest,
    history::{
        HistoryEvidenceError, NativeHistoryCoverage, NativeHistoryEvidenceV1, NativeHistoryRow,
        NativeHistoryState,
    },
    inspection::{
        InspectionField as Field, InspectionGap, InspectionGapReason as Reason, InspectionGapScope,
        InspectionScope, Observation, TargetId,
    },
    migration::{MigrationAdapter, MigrationDirection, MigrationIdentity},
    snapshot::QualifiedName,
};
use thiserror::Error;

use crate::{
    InspectionTarget, InspectionTimeouts, TransportStage,
    transport::{InspectionIsolation, TransportError},
};

mod profile;
mod reader;

use profile::{HistoryProfile, RowEvent};

const DEFAULT_ROW_LIMIT: u16 = 2048;
const MAX_ROW_LIMIT: u16 = 8192;

/// A single history table identified by the protected configuration for a runner.
#[derive(Debug, Clone, Eq, PartialEq)]
pub struct HistoryOptions {
    expected_major: u32,
    adapter: MigrationAdapter,
    source: QualifiedName,
    row_limit: u16,
}

impl HistoryOptions {
    pub fn new(
        expected_major: u32,
        adapter: MigrationAdapter,
        source: QualifiedName,
    ) -> Result<Self, HistoryError> {
        if !(15..=18).contains(&expected_major) {
            return Err(HistoryError::UnsupportedMajor);
        }
        if adapter == MigrationAdapter::Plain {
            return Err(HistoryError::UnsupportedAdapter);
        }
        if InspectionScope::new(vec![source.schema.clone()]).is_err()
            || source.name.is_empty()
            || source.name.len() > 63
            || source.name.chars().any(char::is_control)
        {
            return Err(HistoryError::InvalidSource);
        }
        Ok(Self {
            expected_major,
            adapter,
            source,
            row_limit: DEFAULT_ROW_LIMIT,
        })
    }

    /// Maximum accepted records. The reader fetches one additional row to detect truncation.
    pub fn with_row_limit(mut self, limit: u16) -> Result<Self, HistoryError> {
        if !(1..=MAX_ROW_LIMIT).contains(&limit) {
            return Err(HistoryError::InvalidRowLimit);
        }
        self.row_limit = limit;
        Ok(self)
    }
}

#[derive(Debug, Error)]
pub enum HistoryError {
    #[error("history inspection supports PostgreSQL 15 through 18")]
    UnsupportedMajor,
    #[error("plain SQL uses an applied manifest instead of native history")]
    UnsupportedAdapter,
    #[error("native history needs an exact qualified name in a non-system schema")]
    InvalidSource,
    #[error("native history row limit must be 1 to 8192")]
    InvalidRowLimit,
    #[error("native history observation time precedes the Unix epoch")]
    Clock,
    #[error(transparent)]
    Evidence(#[from] HistoryEvidenceError),
}

impl InspectionTarget {
    /// Inspect the explicitly authorized native history table over verified TLS.
    ///
    /// The caller supplies the inventory digest computed from the discovered forward files.
    /// An unavailable read returns typed evidence for that source without observed rows.
    pub async fn inspect_history(
        &self,
        target_id: TargetId,
        inventory_digest: ContentDigest,
        options: &HistoryOptions,
        timeouts: InspectionTimeouts,
    ) -> Result<NativeHistoryEvidenceV1, HistoryError> {
        let observed_at_unix_seconds = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_err(|_| HistoryError::Clock)?
            .as_secs();
        let mut server_version_num = None;
        let profile = HistoryProfile::for_adapter(options.adapter);
        let result = self
            .with_read_only_at(
                InspectionIsolation::History,
                timeouts,
                async |tx, server| {
                    server_version_num = Some(server.server_version_num);
                    if server.server_version_num / 10_000 != options.expected_major {
                        return Ok(HistoryProjection::unavailable(
                            Field::ServerVersion,
                            Reason::Unsupported,
                        ));
                    }
                    Ok(match reader::read(tx, options, profile).await {
                        Ok(rows) => HistoryProjection::from_rows(rows, options, profile),
                        Err(error) => {
                            HistoryProjection::unavailable(Field::History, error.reason())
                        }
                    })
                },
            )
            .await;

        let projection = match result {
            Ok(projection) => projection,
            Err(error) => {
                let field = if matches!(
                    error,
                    TransportError::Postgres {
                        stage: TransportStage::Rollback,
                        ..
                    }
                ) {
                    Field::Connection
                } else if server_version_num.is_some() {
                    Field::History
                } else {
                    Field::Connection
                };
                HistoryProjection::unavailable(field, error.gap_reason())
            }
        };
        Ok(NativeHistoryEvidenceV1::new(
            Observation {
                target_id,
                server_version_num,
                observed_at_unix_seconds,
            },
            options.adapter,
            Some(options.source.clone()),
            inventory_digest,
            projection.coverage,
            projection.rows,
            projection.gaps,
        )?)
    }
}

struct HistoryProjection {
    coverage: NativeHistoryCoverage,
    rows: Vec<NativeHistoryRow>,
    gaps: Vec<InspectionGap>,
}

impl HistoryProjection {
    fn unavailable(field: Field, reason: Reason) -> Self {
        Self {
            coverage: NativeHistoryCoverage::Unavailable,
            rows: Vec::new(),
            gaps: vec![target_gap(field, reason)],
        }
    }

    fn from_rows(
        rows: Vec<tokio_postgres::Row>,
        options: &HistoryOptions,
        profile: HistoryProfile,
    ) -> Self {
        let limit = usize::from(options.row_limit);
        let mut projection = Self {
            coverage: NativeHistoryCoverage::Complete,
            rows: Vec::with_capacity(rows.len().min(limit)),
            gaps: Vec::new(),
        };
        if rows.len() > limit {
            projection.record_gap(Reason::Truncated);
        }
        let mut bootstrap = false;
        let mut versions = BTreeSet::new();
        let mut ranks = BTreeSet::new();
        for row in rows.iter().take(limit) {
            match profile.event(row) {
                Ok(RowEvent::Bootstrap) if !bootstrap => bootstrap = true,
                Ok(RowEvent::Bootstrap) => projection.record_gap(Reason::InvalidValue),
                Ok(RowEvent::Migration(row)) => {
                    let unique = match row.rank {
                        Some(rank) => ranks.insert(rank),
                        None => versions.insert(row.identity.clone()),
                    };
                    if !unique {
                        projection.record_gap(Reason::InvalidValue);
                        continue;
                    }
                    projection.rows.push(row);
                }
                Err(reason) => projection.record_gap(reason),
            }
        }
        if options.adapter == MigrationAdapter::Goose && !bootstrap {
            projection.record_gap(Reason::InvalidValue);
        }
        projection.record_checksum_gaps(options.adapter);
        projection
    }

    fn record_checksum_gaps(&mut self, adapter: MigrationAdapter) {
        if !matches!(adapter, MigrationAdapter::Sqlx | MigrationAdapter::Flyway) {
            return;
        }
        let mut latest = BTreeMap::<&MigrationIdentity, &NativeHistoryRow>::new();
        for row in &self.rows {
            latest
                .entry(&row.identity)
                .and_modify(|previous| {
                    if row.rank > previous.rank {
                        *previous = row;
                    }
                })
                .or_insert(row);
        }
        for row in latest.into_values() {
            if row.state == NativeHistoryState::Applied
                && row.direction == MigrationDirection::Forward
                && row.checksum.is_none()
            {
                self.gaps.push(InspectionGap {
                    scope: InspectionGapScope::Migration {
                        identity: row.identity.clone(),
                    },
                    field: Field::NativeChecksum,
                    reason: Reason::InvalidValue,
                });
            }
        }
    }

    fn record_gap(&mut self, reason: Reason) {
        self.coverage = NativeHistoryCoverage::Incomplete;
        self.gaps.push(target_gap(Field::History, reason));
    }
}

fn target_gap(field: Field, reason: Reason) -> InspectionGap {
    InspectionGap {
        scope: InspectionGapScope::Target,
        field,
        reason,
    }
}

#[cfg(test)]
mod tests;
