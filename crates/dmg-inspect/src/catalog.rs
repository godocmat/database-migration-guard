use std::{
    collections::BTreeMap,
    time::{SystemTime, UNIX_EPOCH},
};

use dmg_core::{
    inspection::{
        InspectionEvidenceError, InspectionEvidenceV1, InspectionField, InspectionGap,
        InspectionGapReason, InspectionGapScope, InspectionScope, Observation, SchemaObservation,
        SchemaState, TargetId,
    },
    snapshot::{DependencyEdge, PartitionEdge, SchemaSnapshotV1, SnapshotError},
};
use thiserror::Error;
use tokio_postgres::{
    Row, Transaction,
    types::{FromSqlOwned, ToSql, Type},
};

use crate::{InspectionTarget, InspectionTimeouts, ServerInfo, TransportError, TransportStage};

mod details;
mod structure;

use details::PendingDetails;
use structure::{PROJECTIONS, Relation};

const SCOPE: &str = include_str!("catalog/sql/scope.sql");
const DEFAULT_ROW_LIMIT: u16 = 2048;
const MAX_ROW_LIMIT: u16 = 8192;

/// Optional protected definition text and observed relation size. Both default to off.
#[derive(Debug, Default, Clone, Copy, Eq, PartialEq)]
pub struct CatalogDetails {
    pub definitions: bool,
    pub total_bytes: bool,
}

/// Expected server shape and bounds for a fixed catalog inspection.
#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub struct CatalogOptions {
    expected_major: u32,
    row_limit: u16,
    details: CatalogDetails,
}

impl CatalogOptions {
    pub fn new(expected_major: u32) -> Result<Self, CatalogError> {
        if !(15..=18).contains(&expected_major) {
            return Err(CatalogError::UnsupportedMajor);
        }
        Ok(Self {
            expected_major,
            row_limit: DEFAULT_ROW_LIMIT,
            details: CatalogDetails::default(),
        })
    }

    /// Maximum accepted rows per structural projection, from 1 to 8192.
    pub fn with_row_limit(mut self, limit: u16) -> Result<Self, CatalogError> {
        if !(1..=MAX_ROW_LIMIT).contains(&limit) {
            return Err(CatalogError::InvalidRowLimit);
        }
        self.row_limit = limit;
        Ok(self)
    }

    pub fn with_details(mut self, details: CatalogDetails) -> Self {
        self.details = details;
        self
    }
}

#[derive(Debug, Error, Eq, PartialEq)]
pub enum CatalogError {
    #[error("catalog inspection supports PostgreSQL 15 through 18")]
    UnsupportedMajor,
    #[error("catalog projection row limit must be 1 to 8192")]
    InvalidRowLimit,
    #[error("catalog observation time precedes the Unix epoch")]
    Clock,
    #[error(transparent)]
    Snapshot(#[from] SnapshotError),
    #[error(transparent)]
    Evidence(#[from] InspectionEvidenceError),
}

impl InspectionTarget {
    /// Read fixed PostgreSQL catalog projections for the explicitly authorized schemas.
    ///
    /// A failed or incomplete observation is represented by typed gaps. If enabled,
    /// definition text belongs in protected evidence and may contain sensitive literals.
    pub async fn inspect_catalog(
        &self,
        target_id: TargetId,
        scope: &InspectionScope,
        options: CatalogOptions,
        timeouts: InspectionTimeouts,
    ) -> Result<InspectionEvidenceV1, CatalogError> {
        let observed_at_unix_seconds = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_err(|_| CatalogError::Clock)?
            .as_secs();
        let mut collection = Collection::new(scope, options);
        let result = self
            .with_read_only(timeouts, async |tx, server| {
                collection.collect(tx, server).await
            })
            .await;
        if let Err(error) = result {
            let reason = error.gap_reason();
            collection
                .gaps
                .push(target_gap(InspectionField::Connection, reason));
            collection.note_interruption(reason);
        }

        collection.into_evidence(target_id, observed_at_unix_seconds)
    }
}

type Oid = u32;
type CatalogValue<T> = Result<T, InspectionGapReason>;
type Parameter<'a> = (&'a (dyn ToSql + Sync), Type);

pub(crate) struct Collection {
    server: Option<ServerInfo>,
    scope: Vec<SchemaObservation>,
    namespaces: BTreeMap<Oid, String>,
    pending_projections: Vec<InspectionField>,
    options: CatalogOptions,
    pending_details: PendingDetails,
    relations: BTreeMap<Oid, Relation>,
    relation_order: Vec<Oid>,
    partition_edges: Vec<PartitionEdge>,
    dependencies: Vec<DependencyEdge>,
    structural_gaps: Vec<InspectionGap>,
    gaps: Vec<InspectionGap>,
}

impl Collection {
    pub(crate) fn new(scope: &InspectionScope, options: CatalogOptions) -> Self {
        Self {
            server: None,
            scope: scope
                .schemas()
                .iter()
                .map(|schema| SchemaObservation {
                    schema: schema.clone(),
                    state: SchemaState::Unavailable,
                })
                .collect(),
            namespaces: BTreeMap::new(),
            pending_projections: PROJECTIONS.to_vec(),
            options,
            pending_details: PendingDetails::default(),
            relations: BTreeMap::new(),
            relation_order: Vec::new(),
            partition_edges: Vec::new(),
            dependencies: Vec::new(),
            structural_gaps: Vec::new(),
            gaps: Vec::new(),
        }
    }

    /// Collect within a transaction whose read-only settings and server info were verified.
    pub(crate) async fn collect(
        &mut self,
        tx: &mut Transaction<'_>,
        server: ServerInfo,
    ) -> Result<(), TransportError> {
        self.server = Some(server);
        if server.server_version_num / 10_000 != self.options.expected_major {
            self.gaps.push(target_gap(
                InspectionField::ServerVersion,
                InspectionGapReason::Unsupported,
            ));
            return Ok(());
        }
        self.collect_scope(tx).await?;
        if !self.namespaces.is_empty() {
            self.collect_structure(tx, self.options.row_limit, server)
                .await?;
            self.collect_details(tx).await?;
        }
        Ok(())
    }

    fn note_interruption(&mut self, reason: InspectionGapReason) {
        if !self.namespaces.is_empty() {
            self.structural_gaps.extend(
                self.pending_projections
                    .drain(..)
                    .map(|field| target_gap(field, reason)),
            );
            self.note_interrupted_details(reason);
        }
    }

    async fn collect_scope(&mut self, tx: &mut Transaction<'_>) -> Result<(), TransportError> {
        let names: Vec<_> = self
            .scope
            .iter()
            .map(|entry| entry.schema.as_str())
            .collect();
        match isolated_query(tx, SCOPE, &[(&names, Type::TEXT_ARRAY)]).await? {
            Ok(rows) => {
                for row in rows {
                    if let Err(reason) = self.record_scope(&row) {
                        self.gaps.push(target_gap(InspectionField::Scope, reason));
                    }
                }
            }
            Err(reason) => self.gaps.push(target_gap(InspectionField::Scope, reason)),
        }
        Ok(())
    }

    fn record_scope(&mut self, row: &Row) -> CatalogValue<()> {
        let schema: &str = row
            .try_get(0)
            .map_err(|_| InspectionGapReason::InvalidValue)?;
        let index = self
            .scope
            .binary_search_by(|entry| entry.schema.as_str().cmp(schema))
            .map_err(|_| InspectionGapReason::InvalidValue)?;
        let entry = &mut self.scope[index];
        let state = match (
            value::<Option<Oid>>(row, 1)?,
            value::<Option<bool>>(row, 2)?,
        ) {
            (Some(oid), Some(true)) => {
                self.namespaces.insert(oid, entry.schema.clone());
                SchemaState::Visible
            }
            (Some(_), Some(false)) => {
                self.gaps.push(schema_gap(
                    schema,
                    InspectionField::Scope,
                    InspectionGapReason::PermissionDenied,
                ));
                SchemaState::Denied
            }
            (None, None) => {
                self.gaps.push(schema_gap(
                    schema,
                    InspectionField::Scope,
                    InspectionGapReason::Missing,
                ));
                SchemaState::Missing
            }
            _ => return Err(InspectionGapReason::InvalidValue),
        };
        entry.state = state;
        Ok(())
    }

    async fn projection(
        &mut self,
        tx: &mut Transaction<'_>,
        query: &str,
        params: &[Parameter<'_>],
        field: InspectionField,
        limit: usize,
    ) -> Result<Vec<Row>, TransportError> {
        let result = isolated_query(tx, query, params).await?;
        self.pending_projections.retain(|&pending| pending != field);
        match result {
            Ok(mut rows) => {
                if rows.len() > limit {
                    rows.truncate(limit);
                    self.structural_gaps
                        .push(target_gap(field, InspectionGapReason::Truncated));
                }
                Ok(rows)
            }
            Err(reason) => {
                self.structural_gaps.push(target_gap(field, reason));
                Ok(Vec::new())
            }
        }
    }

    pub(crate) fn into_evidence(
        self,
        target_id: TargetId,
        observed_at_unix_seconds: u64,
    ) -> Result<InspectionEvidenceV1, CatalogError> {
        let mut snapshot = None;
        let mut relation_estimates = Vec::new();
        if !self.namespaces.is_empty() {
            let (tables, estimates) = self
                .relations
                .into_values()
                .map(|relation| (relation.table, relation.estimates))
                .unzip();
            snapshot = Some(SchemaSnapshotV1::new(
                self.namespaces.into_values().collect(),
                tables,
                self.partition_edges,
                self.dependencies,
                self.structural_gaps,
            )?);
            relation_estimates = estimates;
        }

        Ok(InspectionEvidenceV1::new(
            Observation {
                target_id,
                server_version_num: self.server.map(|server| server.server_version_num),
                observed_at_unix_seconds,
            },
            self.scope,
            snapshot,
            relation_estimates,
            None,
            self.gaps,
        )?)
    }
}

async fn isolated_query(
    tx: &mut Transaction<'_>,
    query: &str,
    params: &[Parameter<'_>],
) -> Result<CatalogValue<Vec<Row>>, TransportError> {
    let savepoint = tx
        .savepoint("dmg_catalog_read")
        .await
        .map_err(|error| TransportError::postgres(TransportStage::Query, &error))?;
    let result = savepoint
        .query_typed(query, params)
        .await
        .map_err(|error| TransportError::postgres(TransportStage::Query, &error).gap_reason());
    // Rolling back even successful reads releases locks held by an optional function.
    savepoint
        .rollback()
        .await
        .map_err(|error| TransportError::postgres(TransportStage::Rollback, &error))?;
    tx.batch_execute("RELEASE SAVEPOINT dmg_catalog_read")
        .await
        .map_err(|error| TransportError::postgres(TransportStage::Rollback, &error))?;
    Ok(result)
}

fn value<T: FromSqlOwned>(row: &Row, index: usize) -> CatalogValue<T> {
    row.try_get(index)
        .map_err(|_| InspectionGapReason::InvalidValue)
}

fn target_gap(field: InspectionField, reason: InspectionGapReason) -> InspectionGap {
    InspectionGap {
        scope: InspectionGapScope::Target,
        field,
        reason,
    }
}

fn schema_gap(schema: &str, field: InspectionField, reason: InspectionGapReason) -> InspectionGap {
    InspectionGap {
        scope: InspectionGapScope::Schema {
            schema: schema.into(),
        },
        field,
        reason,
    }
}

#[cfg(test)]
mod tests;
