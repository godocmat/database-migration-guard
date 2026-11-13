//! Canonical structural facts for the supported PostgreSQL catalog scope.

use std::collections::BTreeSet;

use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::{
    digest::ContentDigest,
    inspection::{
        CatalogField, InspectionGap, canonicalize_schema_names, gaps_reference_only_schemas,
        is_catalog_identifier,
    },
    risk::{DatabaseObject, ObjectKind},
    schema::SchemaVersionV1,
};

pub const SNAPSHOT_SCHEMA_VERSION: u32 = SchemaVersionV1::NUMBER;

/// Exact catalog names, including case and characters allowed in quoted identifiers.
#[derive(Debug, Clone, Eq, PartialEq, Ord, PartialOrd, Hash, Serialize, Deserialize)]
pub struct QualifiedName {
    pub schema: String,
    pub name: String,
}

impl QualifiedName {
    pub(crate) fn is_valid(&self) -> bool {
        is_catalog_identifier(&self.schema) && is_catalog_identifier(&self.name)
    }
}

#[derive(Debug, Clone, Eq, PartialEq, Serialize, Deserialize)]
pub struct TableSnapshot {
    pub name: QualifiedName,
    pub kind: TableKind,
    pub is_partition: bool,
    /// Sorted by physical attribute position. Dropped columns leave position gaps.
    pub columns: Vec<ColumnSnapshot>,
    pub indexes: Vec<IndexSnapshot>,
    pub constraints: Vec<ConstraintSnapshot>,
}

#[derive(Debug, Clone, Copy, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TableKind {
    Ordinary,
    Partitioned,
}

#[derive(Debug, Clone, Eq, PartialEq, Serialize, Deserialize)]
pub struct ColumnSnapshot {
    pub name: String,
    pub position: u16,
    pub data_type: QualifiedName,
    pub type_modifier: i32,
    pub array_dimensions: u16,
    pub not_null: bool,
    pub identity: Option<ColumnIdentity>,
    pub generation: Option<ColumnGeneration>,
    /// A pg_attrdef entry for a default or generated expression.
    pub has_default: bool,
    /// Protected catalog text. No expression uses NotApplicable.
    pub default_definition: CatalogField<String>,
}

#[derive(Debug, Clone, Copy, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ColumnIdentity {
    Always,
    ByDefault,
}

#[derive(Debug, Clone, Copy, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ColumnGeneration {
    Stored,
    Virtual,
}

#[derive(Debug, Clone, Eq, PartialEq, Serialize, Deserialize)]
pub struct IndexSnapshot {
    pub name: String,
    pub access_method: String,
    pub unique: bool,
    pub primary: bool,
    pub valid: bool,
    pub ready: bool,
    pub nulls_not_distinct: bool,
    pub partial: bool,
    /// Key and include order follow the catalog, with expressions as explicit keys.
    pub keys: Vec<IndexKey>,
    pub included_columns: Vec<String>,
    /// Protected catalog text.
    pub definition: CatalogField<String>,
}

#[derive(Debug, Clone, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum IndexKey {
    Column { name: String },
    Expression,
}

#[derive(Debug, Clone, Eq, PartialEq, Serialize, Deserialize)]
pub struct ConstraintSnapshot {
    pub name: String,
    pub kind: ConstraintKind,
    pub columns: Vec<String>,
    pub validated: bool,
    /// Older majors can report true for the supported kinds.
    pub enforced: bool,
    pub deferrable: bool,
    pub initially_deferred: bool,
    pub references: Option<ForeignKeyTarget>,
    /// Protected catalog text.
    pub definition: CatalogField<String>,
}

#[derive(Debug, Clone, Copy, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ConstraintKind {
    Check,
    NotNull,
    PrimaryKey,
    Unique,
    ForeignKey,
    Exclusion,
}

#[derive(Debug, Clone, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum ForeignKeyTarget {
    Observed {
        table: QualifiedName,
        columns: Vec<String>,
    },
    OutsideScope,
    Unavailable,
}

#[derive(Debug, Clone, Eq, PartialEq, Ord, PartialOrd, Serialize, Deserialize)]
pub struct PartitionEdge {
    pub child: QualifiedName,
    pub parent: QualifiedName,
    pub detach_pending: bool,
}

/// Both endpoints are qualified within the observed structural scope.
#[derive(Debug, Clone, Eq, PartialEq, Ord, PartialOrd, Serialize, Deserialize)]
pub struct DependencyEdge {
    pub dependent: DatabaseObject,
    pub referenced: DatabaseObject,
    pub kind: DependencyKind,
}

#[derive(Debug, Clone, Copy, Eq, PartialEq, Ord, PartialOrd, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DependencyKind {
    Normal,
    Automatic,
    Internal,
}

/// A canonicalized snapshot of visible schemas. Gaps limit what its payload establishes.
#[derive(Debug, Clone, Eq, PartialEq, Serialize, Deserialize)]
#[serde(try_from = "SnapshotFields")]
pub struct SchemaSnapshotV1 {
    schema_version: SchemaVersionV1,
    schemas: Vec<String>,
    tables: Vec<TableSnapshot>,
    partition_edges: Vec<PartitionEdge>,
    dependencies: Vec<DependencyEdge>,
    gaps: Vec<InspectionGap>,
}

impl SchemaSnapshotV1 {
    pub fn new(
        mut schemas: Vec<String>,
        mut tables: Vec<TableSnapshot>,
        mut partition_edges: Vec<PartitionEdge>,
        mut dependencies: Vec<DependencyEdge>,
        mut gaps: Vec<InspectionGap>,
    ) -> Result<Self, SnapshotError> {
        if canonicalize_schema_names(&mut schemas).is_err() {
            return Err(SnapshotError::InvalidSchema);
        }
        let includes = |schema: &String| schemas.binary_search(schema).is_ok();
        tables.sort_unstable_by(|left, right| left.name.cmp(&right.name));
        for table in &mut tables {
            if !table.name.is_valid() || !includes(&table.name.schema) {
                return Err(SnapshotError::InvalidTable);
            }
            canonicalize_table(table, includes)?;
        }
        if tables.windows(2).any(|pair| pair[0].name == pair[1].name) {
            return Err(SnapshotError::DuplicateTable);
        }
        let mut relation_names = tables
            .iter()
            .map(|table| (&table.name.schema, &table.name.name))
            .collect::<BTreeSet<_>>();
        for table in &tables {
            if table
                .indexes
                .iter()
                .any(|index| !relation_names.insert((&table.name.schema, &index.name)))
            {
                return Err(SnapshotError::InvalidIndex);
            }
        }

        partition_edges.sort_unstable();
        if partition_edges.iter().any(|edge| {
            edge.child == edge.parent
                || [&edge.child, &edge.parent].iter().any(|name| {
                    tables
                        .binary_search_by(|table| table.name.cmp(name))
                        .is_err()
                })
        }) || partition_edges
            .windows(2)
            .any(|pair| pair[0].child == pair[1].child && pair[0].parent == pair[1].parent)
        {
            return Err(SnapshotError::InvalidPartitionEdge);
        }

        if dependencies.iter().any(|edge| {
            !dependency_endpoint_is_scoped(&edge.dependent, includes)
                || !dependency_endpoint_is_scoped(&edge.referenced, includes)
        }) {
            return Err(SnapshotError::InvalidDependency);
        }
        dependencies.sort_unstable();
        dependencies.dedup();
        if !gaps_reference_only_schemas(&gaps, &schemas) {
            return Err(SnapshotError::InvalidGapScope);
        }
        gaps.sort_unstable();
        gaps.dedup();
        Ok(Self {
            schema_version: SchemaVersionV1,
            schemas,
            tables,
            partition_edges,
            dependencies,
            gaps,
        })
    }

    pub fn schemas(&self) -> &[String] {
        &self.schemas
    }

    pub fn tables(&self) -> &[TableSnapshot] {
        &self.tables
    }

    pub fn table(&self, name: &QualifiedName) -> Option<&TableSnapshot> {
        let index = self
            .tables
            .binary_search_by(|table| table.name.cmp(name))
            .ok()?;
        self.tables.get(index)
    }

    pub fn partition_edges(&self) -> &[PartitionEdge] {
        &self.partition_edges
    }

    pub fn dependencies(&self) -> &[DependencyEdge] {
        &self.dependencies
    }

    pub fn gaps(&self) -> &[InspectionGap] {
        &self.gaps
    }

    /// SHA-256 of the v1 structural payload, including field availability.
    pub fn digest(&self) -> Result<ContentDigest, serde_json::Error> {
        #[derive(Serialize)]
        struct Structure<'a> {
            schema_version: SchemaVersionV1,
            schemas: &'a [String],
            tables: &'a [TableSnapshot],
            partition_edges: &'a [PartitionEdge],
            dependencies: &'a [DependencyEdge],
        }
        let bytes = serde_json::to_vec(&Structure {
            schema_version: self.schema_version,
            schemas: self.schemas(),
            tables: self.tables(),
            partition_edges: self.partition_edges(),
            dependencies: self.dependencies(),
        })?;
        Ok(ContentDigest::of(&bytes))
    }
}

fn canonicalize_table(
    table: &mut TableSnapshot,
    includes: impl Fn(&String) -> bool,
) -> Result<(), SnapshotError> {
    table.columns.sort_unstable_by_key(|column| column.position);
    let mut column_names = BTreeSet::new();
    for column in &table.columns {
        if column.position == 0
            || !column_names.insert(&column.name)
            || !is_catalog_identifier(&column.name)
            || !column.data_type.is_valid()
            || (column.generation.is_some() && (!column.has_default || column.identity.is_some()))
            || !definition_is_valid(&column.default_definition, column.has_default)
        {
            return Err(SnapshotError::InvalidColumn);
        }
    }
    if table
        .columns
        .windows(2)
        .any(|pair| pair[0].position == pair[1].position)
    {
        return Err(SnapshotError::InvalidColumn);
    }

    table
        .indexes
        .sort_unstable_by(|left, right| left.name.cmp(&right.name));
    if table.indexes.iter().any(|index| {
        !is_catalog_identifier(&index.name)
            || !is_catalog_identifier(&index.access_method)
            || index
                .keys
                .iter()
                .any(|key| matches!(key, IndexKey::Column { name } if !is_catalog_identifier(name)))
            || !identifiers_are_valid(&index.included_columns)
            || !definition_is_valid(&index.definition, true)
    }) || table
        .indexes
        .windows(2)
        .any(|pair| pair[0].name == pair[1].name)
    {
        return Err(SnapshotError::InvalidIndex);
    }

    table
        .constraints
        .sort_unstable_by(|left, right| left.name.cmp(&right.name));
    if table.constraints.iter().any(|constraint| {
        !is_catalog_identifier(&constraint.name)
            || !identifiers_are_valid(&constraint.columns)
            || !definition_is_valid(&constraint.definition, true)
            || (constraint.kind == ConstraintKind::ForeignKey) != constraint.references.is_some()
            || matches!(&constraint.references, Some(ForeignKeyTarget::Observed { table, columns })
                if !table.is_valid()
                    || !includes(&table.schema)
                    || !identifiers_are_valid(columns)
                    || columns.len() != constraint.columns.len())
    }) || table
        .constraints
        .windows(2)
        .any(|pair| pair[0].name == pair[1].name)
    {
        return Err(SnapshotError::InvalidConstraint);
    }
    Ok(())
}

fn identifiers_are_valid(names: &[String]) -> bool {
    names.iter().all(|name| is_catalog_identifier(name))
}

fn definition_is_valid(definition: &CatalogField<String>, exists: bool) -> bool {
    match definition {
        CatalogField::Observed { value } => exists && !value.is_empty() && !value.contains('\0'),
        CatalogField::Unavailable => exists,
        CatalogField::NotApplicable => !exists,
        CatalogField::Estimated { .. } => false,
    }
}

fn dependency_endpoint_is_scoped(
    object: &DatabaseObject,
    includes: impl Fn(&String) -> bool,
) -> bool {
    if !is_catalog_identifier(&object.name) {
        return false;
    }
    match object.kind {
        ObjectKind::Schema => {
            object.schema.is_none() && object.member.is_none() && includes(&object.name)
        }
        ObjectKind::Relation | ObjectKind::Type => {
            object.member.is_none() && object.schema.as_ref().is_some_and(includes)
        }
        ObjectKind::Column | ObjectKind::Constraint => {
            object
                .member
                .as_ref()
                .is_some_and(|name| is_catalog_identifier(name))
                && object.schema.as_ref().is_some_and(includes)
        }
        ObjectKind::Index => {
            object
                .member
                .as_ref()
                .is_none_or(|name| is_catalog_identifier(name))
                && object.schema.as_ref().is_some_and(includes)
        }
    }
}

#[derive(Deserialize)]
struct SnapshotFields {
    #[serde(rename = "schema_version")]
    _schema_version: SchemaVersionV1,
    schemas: Vec<String>,
    tables: Vec<TableSnapshot>,
    partition_edges: Vec<PartitionEdge>,
    dependencies: Vec<DependencyEdge>,
    gaps: Vec<InspectionGap>,
}

impl TryFrom<SnapshotFields> for SchemaSnapshotV1 {
    type Error = SnapshotError;

    fn try_from(fields: SnapshotFields) -> Result<Self, Self::Error> {
        Self::new(
            fields.schemas,
            fields.tables,
            fields.partition_edges,
            fields.dependencies,
            fields.gaps,
        )
    }
}

#[derive(Debug, Error, Eq, PartialEq)]
pub enum SnapshotError {
    #[error("snapshot contains invalid or duplicate visible schema names")]
    InvalidSchema,
    #[error("snapshot contains a table outside its visible schemas or with an invalid name")]
    InvalidTable,
    #[error("snapshot contains a duplicate qualified table")]
    DuplicateTable,
    #[error("snapshot contains an invalid column or duplicate column identity")]
    InvalidColumn,
    #[error("snapshot contains an invalid index or duplicate index identity")]
    InvalidIndex,
    #[error("snapshot contains an invalid constraint or duplicate constraint identity")]
    InvalidConstraint,
    #[error("snapshot contains an invalid or duplicate partition edge")]
    InvalidPartitionEdge,
    #[error("snapshot contains a dependency endpoint outside the observed scope")]
    InvalidDependency,
    #[error("snapshot contains a gap outside its visible schemas")]
    InvalidGapScope,
}
