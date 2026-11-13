use std::collections::{BTreeMap, BTreeSet};

use crate::{
    inspection::{InspectionGapReason, InspectionGapScope, SchemaObservation},
    snapshot::{
        ColumnSnapshot, ConstraintSnapshot, DependencyKind, ForeignKeyTarget, IndexSnapshot,
        TableSnapshot,
    },
};

use super::*;

#[derive(Clone, Copy)]
pub(super) enum Record<'a> {
    Table(&'a TableSnapshot),
    Column(&'a TableSnapshot, &'a ColumnSnapshot),
    Index(&'a TableSnapshot, &'a IndexSnapshot),
    Constraint(&'a TableSnapshot, &'a ConstraintSnapshot),
}

impl<'a> Record<'a> {
    pub(super) fn owner(self) -> Option<&'a QualifiedName> {
        match self {
            Self::Table(_) => None,
            Self::Column(table, _) | Self::Index(table, _) | Self::Constraint(table, _) => {
                Some(&table.name)
            }
        }
    }

    pub(super) fn change_object(self, identity: &DatabaseObject) -> ChangeObject {
        ChangeObject {
            identity: identity.clone(),
            owning_table: self.owner().cloned(),
        }
    }
}

#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub(crate) enum Presence {
    Present,
    Absent,
    Unknown,
}

pub(crate) struct Inventory<'a> {
    pub(super) input: SchemaInput<'a>,
    pub(super) objects: BTreeMap<DatabaseObject, Record<'a>>,
    gaps: GapIndex<'a>,
}

impl<'a> Inventory<'a> {
    pub(crate) fn new(input: SchemaInput<'a>) -> Self {
        let mut objects = BTreeMap::new();
        if let Some(snapshot) = input.snapshot() {
            for table in snapshot.tables() {
                objects.insert(table_id(&table.name), Record::Table(table));
                objects.extend(table.columns.iter().map(|column| {
                    (
                        member_id(ObjectKind::Column, &table.name, &column.name),
                        Record::Column(table, column),
                    )
                }));
                objects.extend(table.indexes.iter().map(|index| {
                    (
                        DatabaseObject {
                            kind: ObjectKind::Index,
                            schema: Some(table.name.schema.clone()),
                            name: index.name.clone(),
                            member: None,
                        },
                        Record::Index(table, index),
                    )
                }));
                objects.extend(table.constraints.iter().map(|constraint| {
                    (
                        member_id(ObjectKind::Constraint, &table.name, &constraint.name),
                        Record::Constraint(table, constraint),
                    )
                }));
            }
        }

        let (snapshot, inspection) = input_gaps(input);
        let mut gaps = GapIndex::default();
        for gap in snapshot.iter().chain(inspection) {
            if !is_verified_missing_schema(input, gap) {
                gaps.insert(gap);
            }
        }
        Self {
            input,
            objects,
            gaps,
        }
    }

    pub(super) fn source(&self) -> Result<DiffSource, DiffError> {
        Ok(DiffSource {
            structural_digest: self
                .input
                .snapshot()
                .map(SchemaSnapshotV1::digest)
                .transpose()?,
            observation: match self.input {
                SchemaInput::Snapshot(_) => None,
                SchemaInput::Inspection(evidence) => Some(evidence.observation().clone()),
            },
        })
    }

    pub(super) fn schemas(&self) -> impl Iterator<Item = &'a str> {
        let (snapshot, observed) = match self.input {
            SchemaInput::Snapshot(snapshot) => (snapshot.schemas(), &[][..]),
            SchemaInput::Inspection(evidence) => (&[][..], evidence.scope()),
        };
        snapshot.iter().map(String::as_str).chain(
            observed
                .iter()
                .map(|entry: &SchemaObservation| entry.schema.as_str()),
        )
    }

    pub(super) fn report_gaps(&self, side: DiffSide) -> Vec<DiffGap> {
        let (snapshot, inspection) = input_gaps(self.input);
        let mut result: Vec<_> = snapshot
            .iter()
            .chain(inspection)
            .filter(|gap| is_structural(gap.field))
            .map(|gap| DiffGap::Inspection {
                side,
                gap: gap.clone(),
            })
            .collect();
        if self.input.snapshot().is_none() {
            result.push(DiffGap::SnapshotUnavailable { side });
        }
        result
    }

    pub(super) fn schema_is_missing(&self, schema: &str) -> bool {
        self.input.scope_state(schema) == ScopeState::Missing
            && !self.gaps.has_broad(InspectionField::Connection, schema)
            && !self.gaps.has_broad(InspectionField::Scope, schema)
    }

    pub(super) fn contains_table(&self, table: &QualifiedName) -> bool {
        self.input
            .snapshot()
            .and_then(|snapshot| snapshot.table(table))
            .is_some()
    }

    fn knows_absence(&self, id: &DatabaseObject, owner: Option<&QualifiedName>) -> bool {
        let Some(schema) = id.schema.as_deref() else {
            return false;
        };
        if self.schema_is_missing(schema) {
            return true;
        }
        if !self.can_read_schema(schema) {
            return false;
        }
        let field = match id.kind {
            ObjectKind::Relation => InspectionField::Relations,
            ObjectKind::Column => InspectionField::Columns,
            ObjectKind::Index => InspectionField::Indexes,
            ObjectKind::Constraint => InspectionField::Constraints,
            _ => return false,
        };
        [field, InspectionField::Scope, InspectionField::Connection]
            .into_iter()
            .all(|field| !self.gaps.has(field, id, owner))
    }

    pub(crate) fn table_presence(&self, table: &QualifiedName) -> Presence {
        self.presence(&table_id(table), None)
    }

    pub(crate) fn column_presence(&self, table: &QualifiedName, name: &str) -> Presence {
        self.presence(&member_id(ObjectKind::Column, table, name), Some(table))
    }

    fn presence(&self, id: &DatabaseObject, owner: Option<&QualifiedName>) -> Presence {
        if self.objects.contains_key(id) {
            return Presence::Present;
        }
        if owner.is_some_and(|table| !self.contains_table(table)) {
            return if owner.is_some_and(|table| self.table_presence(table) == Presence::Absent) {
                Presence::Absent
            } else {
                Presence::Unknown
            };
        }
        if self.knows_absence(id, owner) {
            Presence::Absent
        } else {
            Presence::Unknown
        }
    }

    pub(super) fn proves_absence(
        &self,
        id: &DatabaseObject,
        owner: Option<&QualifiedName>,
    ) -> bool {
        if let (ObjectKind::Column, Some(table), Some(name)) =
            (id.kind, owner, id.member.as_deref())
        {
            return self.column_presence(table, name) == Presence::Absent;
        }
        // An unchanged index name can belong to a different table in each snapshot.
        // The name is the same namespace entry but its ownership distinguishes the objects.
        self.knows_absence(id, owner)
    }

    pub(super) fn partitions(&self) -> &'a [PartitionEdge] {
        self.input
            .snapshot()
            .map_or(&[], SchemaSnapshotV1::partition_edges)
    }

    pub(super) fn dependencies(&self) -> &'a [DependencyEdge] {
        self.input
            .snapshot()
            .map_or(&[], SchemaSnapshotV1::dependencies)
    }

    pub(super) fn knows_partition_absence(
        &self,
        child: &QualifiedName,
        parent: &QualifiedName,
    ) -> bool {
        self.table_is_missing(child)
            || self.table_is_missing(parent)
            || [child, parent].iter().all(|table| {
                self.contains_table(table)
                    && self.can_read_schema(&table.schema)
                    && !self
                        .gaps
                        .has(InspectionField::Partitions, &table_id(table), None)
            })
    }

    pub(super) fn knows_dependency_absence(
        &self,
        edge: &DependencyEdge,
        known_foreign_key_edge: bool,
    ) -> bool {
        if self.endpoint_is_missing(&edge.dependent) || self.endpoint_is_missing(&edge.referenced) {
            return true;
        }
        if known_foreign_key_edge {
            return self.foreign_key_edge_is_absent(edge);
        }
        [&edge.dependent, &edge.referenced].iter().all(|id| {
            let owner = self.objects.get(*id).copied().and_then(Record::owner);
            object_schema(id).is_some_and(|schema| self.can_read_schema(schema))
                && !self.gaps.has(InspectionField::Dependencies, id, owner)
        })
    }

    pub(super) fn is_foreign_key_edge(&self, edge: &DependencyEdge) -> bool {
        edge.kind == DependencyKind::Normal
            && matches!(self.objects.get(&edge.dependent),
            Some(Record::Constraint(_, constraint)) if matches!(
                &constraint.references,
                Some(ForeignKeyTarget::Observed { table, .. }) if table_id(table) == edge.referenced
            ))
    }

    fn foreign_key_edge_is_absent(&self, edge: &DependencyEdge) -> bool {
        match self.objects.get(&edge.dependent) {
            Some(Record::Constraint(_, constraint)) => match &constraint.references {
                None => true,
                Some(ForeignKeyTarget::Observed { table, .. }) => {
                    table_id(table) != edge.referenced
                }
                _ => false,
            },
            None => {
                owning_table(&edge.dependent).is_some_and(|owner| self.table_is_missing(&owner))
            }
            _ => false,
        }
    }

    fn can_read_schema(&self, schema: &str) -> bool {
        self.input.snapshot().is_some()
            && self.input.scope_state(schema) == ScopeState::Visible
            && !self.gaps.has_broad(InspectionField::Connection, schema)
            && !self.gaps.has_broad(InspectionField::Scope, schema)
    }

    fn table_is_missing(&self, table: &QualifiedName) -> bool {
        self.table_presence(table) == Presence::Absent
    }

    fn endpoint_is_missing(&self, id: &DatabaseObject) -> bool {
        match id.kind {
            ObjectKind::Schema => self.schema_is_missing(&id.name),
            // Relation dependencies can reference kinds beyond the table inventory.
            ObjectKind::Type | ObjectKind::Relation => {
                object_schema(id).is_some_and(|schema| self.schema_is_missing(schema))
            }
            _ => {
                object_schema(id).is_some_and(|schema| self.schema_is_missing(schema))
                    || (!self.objects.contains_key(id)
                        && owning_table(id).is_some_and(|owner| self.contains_table(&owner))
                        && self.knows_absence(id, owning_table(id).as_ref()))
            }
        }
    }

    pub(super) fn object_projection_gap(
        &self,
        field: InspectionField,
        object: &ChangeObject,
    ) -> bool {
        self.gaps
            .has_object(field, &object.identity, object.owning_table.as_ref())
    }
}

pub(super) fn table_id(table: &QualifiedName) -> DatabaseObject {
    DatabaseObject {
        kind: ObjectKind::Relation,
        schema: Some(table.schema.clone()),
        name: table.name.clone(),
        member: None,
    }
}

fn member_id(kind: ObjectKind, table: &QualifiedName, name: &str) -> DatabaseObject {
    DatabaseObject {
        kind,
        schema: Some(table.schema.clone()),
        name: table.name.clone(),
        member: Some(name.into()),
    }
}

pub(super) fn object_schema(id: &DatabaseObject) -> Option<&str> {
    if id.kind == ObjectKind::Schema {
        Some(&id.name)
    } else {
        id.schema.as_deref()
    }
}

fn owning_table(id: &DatabaseObject) -> Option<QualifiedName> {
    id.member.as_ref()?;
    Some(QualifiedName {
        schema: id.schema.clone()?,
        name: id.name.clone(),
    })
}

fn input_gaps(input: SchemaInput<'_>) -> (&[InspectionGap], &[InspectionGap]) {
    let snapshot = input.snapshot().map_or(&[][..], SchemaSnapshotV1::gaps);
    let envelope = match input {
        SchemaInput::Snapshot(_) => &[],
        SchemaInput::Inspection(evidence) => evidence.gaps(),
    };
    (snapshot, envelope)
}

pub(super) fn is_verified_missing_schema(input: SchemaInput<'_>, gap: &InspectionGap) -> bool {
    gap.field == InspectionField::Scope
        && gap.reason == InspectionGapReason::Missing
        && matches!(&gap.scope, InspectionGapScope::Schema { schema }
            if input.scope_state(schema) == ScopeState::Missing)
}

fn is_structural(field: InspectionField) -> bool {
    !matches!(
        field,
        InspectionField::PlannerRows
            | InspectionField::EstimatedHeapBytes
            | InspectionField::TotalRelationBytes
            | InspectionField::History
            | InspectionField::NativeChecksum
    )
}

#[derive(Default)]
struct GapIndex<'a> {
    target: BTreeSet<InspectionField>,
    schemas: BTreeMap<&'a str, BTreeSet<InspectionField>>,
    objects: BTreeMap<&'a DatabaseObject, BTreeSet<InspectionField>>,
}

impl<'a> GapIndex<'a> {
    fn insert(&mut self, gap: &'a InspectionGap) {
        let fields = match &gap.scope {
            InspectionGapScope::Target => &mut self.target,
            InspectionGapScope::Schema { schema } => self.schemas.entry(schema).or_default(),
            InspectionGapScope::Object { object } if object.kind == ObjectKind::Schema => {
                self.schemas.entry(&object.name).or_default()
            }
            InspectionGapScope::Object { object } => self.objects.entry(object).or_default(),
            InspectionGapScope::Migration { .. } => return,
        };
        fields.insert(gap.field);
    }

    fn has_broad(&self, field: InspectionField, schema: &str) -> bool {
        self.target.contains(&field)
            || self
                .schemas
                .get(schema)
                .is_some_and(|fields| fields.contains(&field))
    }

    fn has(
        &self,
        field: InspectionField,
        id: &DatabaseObject,
        owner: Option<&QualifiedName>,
    ) -> bool {
        object_schema(id).is_some_and(|schema| self.has_broad(field, schema))
            || self.has_object(field, id, owner)
    }

    fn has_object(
        &self,
        field: InspectionField,
        id: &DatabaseObject,
        owner: Option<&QualifiedName>,
    ) -> bool {
        self.objects
            .get(id)
            .is_some_and(|fields| fields.contains(&field))
            || owner.is_some_and(|table| {
                self.objects
                    .get(&table_id(table))
                    .is_some_and(|fields| fields.contains(&field))
            })
    }
}
