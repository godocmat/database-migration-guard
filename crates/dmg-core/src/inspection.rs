//! Versioned evidence from an authorized read-only PostgreSQL inspection.

use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::{
    history::NativeHistoryEvidenceV1,
    risk::DatabaseObject,
    schema::SchemaVersionV1,
    snapshot::{QualifiedName, SchemaSnapshotV1},
};

pub const INSPECTION_SCHEMA_VERSION: u32 = SchemaVersionV1::NUMBER;

/// A caller-supplied opaque environment identifier. Connection details stay in protected input.
#[derive(Debug, Clone, Eq, PartialEq, Ord, PartialOrd, Hash, Serialize, Deserialize)]
#[serde(try_from = "String")]
pub struct TargetId(String);

impl TargetId {
    pub fn new(id: impl Into<String>) -> Result<Self, InspectionEvidenceError> {
        Self::try_from(id.into())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl TryFrom<String> for TargetId {
    type Error = InspectionEvidenceError;

    fn try_from(id: String) -> Result<Self, Self::Error> {
        if id.len() > 128
            || !id
                .bytes()
                .next()
                .is_some_and(|byte| byte.is_ascii_alphanumeric())
            || !id
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.'))
        {
            return Err(InspectionEvidenceError::InvalidTarget);
        }
        Ok(Self(id))
    }
}

/// The explicitly authorized schema names for one catalog read.
#[derive(Debug, Clone, Eq, PartialEq, Serialize, Deserialize)]
#[serde(try_from = "Vec<String>")]
pub struct InspectionScope(Vec<String>);

impl InspectionScope {
    pub fn new(schemas: Vec<String>) -> Result<Self, InspectionEvidenceError> {
        Self::try_from(schemas)
    }

    pub fn schemas(&self) -> &[String] {
        &self.0
    }
}

impl TryFrom<Vec<String>> for InspectionScope {
    type Error = InspectionEvidenceError;

    fn try_from(mut schemas: Vec<String>) -> Result<Self, Self::Error> {
        if schemas.is_empty() {
            return Err(InspectionEvidenceError::InvalidScope);
        }
        canonicalize_schema_names(&mut schemas)?;
        Ok(Self(schemas))
    }
}

pub(crate) const MAX_INSPECTION_SCHEMAS: usize = 32;

pub(crate) fn is_catalog_identifier(value: &str) -> bool {
    !value.is_empty() && value.len() <= 63 && !value.chars().any(char::is_control)
}

pub(crate) fn canonicalize_schema_names(
    schemas: &mut [String],
) -> Result<(), InspectionEvidenceError> {
    if schemas.len() > MAX_INSPECTION_SCHEMAS {
        return Err(InspectionEvidenceError::InvalidScope);
    }
    schemas.sort_unstable();
    if schemas.iter().any(|schema| {
        !is_catalog_identifier(schema)
            || schema.starts_with("pg_")
            || schema == "information_schema"
    }) || schemas.windows(2).any(|pair| pair[0] == pair[1])
    {
        return Err(InspectionEvidenceError::InvalidScope);
    }
    Ok(())
}

pub(crate) fn gaps_reference_only_schemas(gaps: &[InspectionGap], schemas: &[String]) -> bool {
    gaps.iter().all(|gap| {
        gap.scope.schema_name().is_none_or(|schema| {
            schemas
                .binary_search_by(|name| name.as_str().cmp(schema))
                .is_ok()
        })
    })
}

#[derive(Debug, Clone, Eq, PartialEq, Serialize, Deserialize)]
pub struct Observation {
    pub target_id: TargetId,
    /// Unknown when setup failed before the server version could be read.
    pub server_version_num: Option<u32>,
    /// Seconds since 1970-01-01 00:00:00 UTC.
    pub observed_at_unix_seconds: u64,
}

#[derive(Debug, Clone, Eq, PartialEq, Serialize, Deserialize)]
pub struct SchemaObservation {
    pub schema: String,
    pub state: SchemaState,
}

#[derive(Debug, Clone, Copy, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SchemaState {
    Visible,
    Denied,
    Missing,
    Unavailable,
}

/// A catalog value and the basis for using it in a comparison.
#[derive(Debug, Clone, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum CatalogField<T> {
    Observed { value: T },
    Estimated { value: T },
    Unavailable,
    NotApplicable,
}

#[derive(Debug, Clone, Eq, PartialEq, Serialize, Deserialize)]
pub struct RelationEstimates {
    pub relation: QualifiedName,
    /// Planner estimate, rounded to whole rows. Even zero does not establish emptiness.
    pub rows: CatalogField<u64>,
    /// Estimated bytes from relpages and the server's block size.
    pub heap_bytes: CatalogField<u64>,
    /// Optional observed bytes including indexes and TOAST.
    pub total_bytes: CatalogField<u64>,
}

#[derive(Debug, Clone, Eq, PartialEq, Ord, PartialOrd, Serialize, Deserialize)]
pub struct InspectionGap {
    pub scope: InspectionGapScope,
    pub field: InspectionField,
    pub reason: InspectionGapReason,
}

#[derive(Debug, Clone, Eq, PartialEq, Ord, PartialOrd, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum InspectionGapScope {
    Target,
    Schema {
        schema: String,
    },
    Object {
        object: DatabaseObject,
    },
    Migration {
        identity: crate::migration::MigrationIdentity,
    },
}

impl InspectionGapScope {
    pub(crate) fn schema_name(&self) -> Option<&str> {
        match self {
            Self::Schema { schema } => Some(schema),
            Self::Object { object } if object.kind == crate::risk::ObjectKind::Schema => {
                Some(&object.name)
            }
            Self::Object { object } => object.schema.as_deref(),
            Self::Target | Self::Migration { .. } => None,
        }
    }
}

#[derive(Debug, Clone, Copy, Eq, PartialEq, Ord, PartialOrd, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum InspectionField {
    Connection,
    ServerVersion,
    Scope,
    Relations,
    Columns,
    Indexes,
    Constraints,
    Partitions,
    PartitionBound,
    Dependencies,
    UnsupportedObjects,
    DefaultDefinition,
    IndexDefinition,
    ConstraintDefinition,
    PlannerRows,
    EstimatedHeapBytes,
    TotalRelationBytes,
    History,
    NativeChecksum,
}

#[derive(Debug, Clone, Copy, Eq, PartialEq, Ord, PartialOrd, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum InspectionGapReason {
    PermissionDenied,
    Missing,
    Unsupported,
    Contention,
    Cancelled,
    Deadline,
    Truncated,
    OutsideScope,
    QueryFailed,
    InvalidValue,
}

#[derive(Debug, Clone, Eq, PartialEq, Serialize, Deserialize)]
#[serde(try_from = "InspectionFields")]
pub struct InspectionEvidenceV1 {
    schema_version: SchemaVersionV1,
    observation: Observation,
    scope: Vec<SchemaObservation>,
    snapshot: Option<SchemaSnapshotV1>,
    relation_estimates: Vec<RelationEstimates>,
    history: Option<NativeHistoryEvidenceV1>,
    gaps: Vec<InspectionGap>,
}

impl InspectionEvidenceV1 {
    pub fn new(
        observation: Observation,
        mut scope: Vec<SchemaObservation>,
        snapshot: Option<SchemaSnapshotV1>,
        mut relation_estimates: Vec<RelationEstimates>,
        history: Option<NativeHistoryEvidenceV1>,
        mut gaps: Vec<InspectionGap>,
    ) -> Result<Self, InspectionEvidenceError> {
        let requested =
            InspectionScope::new(scope.iter().map(|entry| entry.schema.clone()).collect())?;
        scope.sort_unstable_by(|left, right| left.schema.cmp(&right.schema));
        let visible = scope
            .iter()
            .filter(|entry| entry.state == SchemaState::Visible)
            .map(|entry| &entry.schema)
            .collect::<Vec<_>>();
        if snapshot
            .as_ref()
            .is_some_and(|value| !value.schemas().iter().eq(visible))
            || (observation.server_version_num.is_none()
                && (snapshot.is_some()
                    || scope
                        .iter()
                        .any(|entry| entry.state != SchemaState::Unavailable)))
        {
            return Err(InspectionEvidenceError::SnapshotScope);
        }

        relation_estimates.sort_unstable_by(|left, right| left.relation.cmp(&right.relation));
        if relation_estimates.iter().any(|entry| {
            !snapshot
                .as_ref()
                .is_some_and(|value| value.table(&entry.relation).is_some())
                || matches!(entry.rows, CatalogField::Observed { .. })
                || matches!(entry.heap_bytes, CatalogField::Observed { .. })
                || matches!(entry.total_bytes, CatalogField::Estimated { .. })
        }) || relation_estimates
            .windows(2)
            .any(|pair| pair[0].relation == pair[1].relation)
        {
            return Err(InspectionEvidenceError::InvalidEstimates);
        }
        if history
            .as_ref()
            .is_some_and(|value| value.observation().target_id != observation.target_id)
        {
            return Err(InspectionEvidenceError::HistoryTargetMismatch);
        }
        if !gaps_reference_only_schemas(&gaps, requested.schemas()) {
            return Err(InspectionEvidenceError::InvalidScope);
        }
        gaps.sort_unstable();
        gaps.dedup();
        Ok(Self {
            schema_version: SchemaVersionV1,
            observation,
            scope,
            snapshot,
            relation_estimates,
            history,
            gaps,
        })
    }

    pub fn observation(&self) -> &Observation {
        &self.observation
    }

    pub fn scope(&self) -> &[SchemaObservation] {
        &self.scope
    }

    pub fn snapshot(&self) -> Option<&SchemaSnapshotV1> {
        self.snapshot.as_ref()
    }

    pub fn relation_estimates(&self) -> &[RelationEstimates] {
        &self.relation_estimates
    }

    pub fn history(&self) -> Option<&NativeHistoryEvidenceV1> {
        self.history.as_ref()
    }

    pub fn gaps(&self) -> &[InspectionGap] {
        &self.gaps
    }
}

#[derive(Deserialize)]
struct InspectionFields {
    #[serde(rename = "schema_version")]
    _schema_version: SchemaVersionV1,
    observation: Observation,
    scope: Vec<SchemaObservation>,
    snapshot: Option<SchemaSnapshotV1>,
    relation_estimates: Vec<RelationEstimates>,
    history: Option<NativeHistoryEvidenceV1>,
    gaps: Vec<InspectionGap>,
}

impl TryFrom<InspectionFields> for InspectionEvidenceV1 {
    type Error = InspectionEvidenceError;

    fn try_from(fields: InspectionFields) -> Result<Self, Self::Error> {
        Self::new(
            fields.observation,
            fields.scope,
            fields.snapshot,
            fields.relation_estimates,
            fields.history,
            fields.gaps,
        )
    }
}

#[derive(Debug, Error, Eq, PartialEq)]
pub enum InspectionEvidenceError {
    #[error(
        "expected an opaque target identifier of at most 128 ASCII letters, digits, dots, dashes, or underscores"
    )]
    InvalidTarget,
    #[error("expected unique authorized non-system schema names")]
    InvalidScope,
    #[error("snapshot schemas do not match the visible inspection scope")]
    SnapshotScope,
    #[error("invalid or duplicate relation estimates for the observed snapshot")]
    InvalidEstimates,
    #[error("native history evidence describes a different target")]
    HistoryTargetMismatch,
}
