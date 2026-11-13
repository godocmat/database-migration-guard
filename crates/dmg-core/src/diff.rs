//! Structural comparison of explicitly scoped PostgreSQL catalog snapshots.

mod fields;
pub(crate) mod input;
mod rename;

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::{
    coverage::CoverageStatus,
    digest::ContentDigest,
    inspection::{InspectionEvidenceV1, InspectionField, InspectionGap, Observation, SchemaState},
    risk::{DatabaseObject, ObjectKind},
    schema::SchemaVersionV1,
    snapshot::{DependencyEdge, PartitionEdge, QualifiedName, SchemaSnapshotV1},
};
use input::{Inventory, Record};
use rename::Renames;

pub const SCHEMA_DIFF_VERSION: u32 = SchemaVersionV1::NUMBER;

/// A saved structural snapshot or an inspection carrying the scope and observation.
#[derive(Debug, Clone, Copy)]
pub enum SchemaInput<'a> {
    Snapshot(&'a SchemaSnapshotV1),
    Inspection(&'a InspectionEvidenceV1),
}

impl<'a> From<&'a SchemaSnapshotV1> for SchemaInput<'a> {
    fn from(value: &'a SchemaSnapshotV1) -> Self {
        Self::Snapshot(value)
    }
}

impl<'a> From<&'a InspectionEvidenceV1> for SchemaInput<'a> {
    fn from(value: &'a InspectionEvidenceV1) -> Self {
        Self::Inspection(value)
    }
}

impl<'a> SchemaInput<'a> {
    pub fn snapshot(self) -> Option<&'a SchemaSnapshotV1> {
        match self {
            Self::Snapshot(snapshot) => Some(snapshot),
            Self::Inspection(inspection) => inspection.snapshot(),
        }
    }

    pub fn scope_state(self, schema: &str) -> ScopeState {
        match self {
            Self::Snapshot(snapshot) => {
                if snapshot
                    .schemas()
                    .binary_search_by(|value| value.as_str().cmp(schema))
                    .is_ok()
                {
                    ScopeState::Visible
                } else {
                    ScopeState::OutsideScope
                }
            }
            Self::Inspection(inspection) => inspection
                .scope()
                .binary_search_by(|value| value.schema.as_str().cmp(schema))
                .ok()
                .map(|index| inspection.scope()[index].state.into())
                .unwrap_or(ScopeState::OutsideScope),
        }
    }
}

#[derive(Debug, Clone, Copy, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ScopeState {
    Visible,
    Missing,
    Denied,
    Unavailable,
    OutsideScope,
}

impl From<SchemaState> for ScopeState {
    fn from(value: SchemaState) -> Self {
        match value {
            SchemaState::Visible => Self::Visible,
            SchemaState::Missing => Self::Missing,
            SchemaState::Denied => Self::Denied,
            SchemaState::Unavailable => Self::Unavailable,
        }
    }
}

#[derive(Debug, Clone, Eq, PartialEq, Serialize, Deserialize)]
pub struct ComparedSchema {
    pub schema: String,
    pub before: ScopeState,
    pub after: ScopeState,
}

#[derive(Debug, Clone, Eq, PartialEq, Serialize, Deserialize)]
pub struct DiffSource {
    pub structural_digest: Option<ContentDigest>,
    pub observation: Option<Observation>,
}

#[derive(Debug, Clone, Copy, Eq, PartialEq, Ord, PartialOrd, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DiffSide {
    Before,
    After,
}

#[derive(Debug, Clone, Eq, PartialEq, Ord, PartialOrd, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum DiffGap {
    EmptyScope,
    SnapshotUnavailable {
        side: DiffSide,
    },
    Inspection {
        side: DiffSide,
        gap: InspectionGap,
    },
    PresenceUnconfirmed {
        missing_from: DiffSide,
        object: ChangeObject,
    },
    PartitionPresenceUnconfirmed {
        missing_from: DiffSide,
        edge: PartitionEdge,
    },
    DependencyPresenceUnconfirmed {
        missing_from: DiffSide,
        edge: DependencyEdge,
    },
    FieldUnavailable {
        side: DiffSide,
        object: ChangeObject,
        field: ChangedField,
    },
}

/// Index names use their schema's relation namespace. The owner distinguishes
/// the same qualified index name on different tables across snapshots.
#[derive(Debug, Clone, Eq, PartialEq, Ord, PartialOrd, Serialize, Deserialize)]
pub struct ChangeObject {
    pub identity: DatabaseObject,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub owning_table: Option<QualifiedName>,
}

#[derive(Debug, Clone, Copy, Eq, PartialEq, Ord, PartialOrd, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ChangedField {
    TableKind,
    PartitionMembership,
    ColumnPosition,
    DataType,
    TypeModifier,
    ArrayDimensions,
    Nullability,
    Identity,
    Generation,
    DefaultPresence,
    DefaultDefinition,
    AccessMethod,
    Unique,
    Primary,
    Valid,
    Ready,
    NullsNotDistinct,
    Partial,
    IndexKeys,
    IncludedColumns,
    IndexDefinition,
    ConstraintKind,
    ConstraintColumns,
    Validated,
    Enforced,
    Deferrable,
    InitiallyDeferred,
    ConstraintReferences,
    ConstraintDefinition,
}

#[derive(Debug, Clone, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum StructuralChange {
    Added {
        object: ChangeObject,
    },
    Removed {
        object: ChangeObject,
    },
    Renamed {
        from: ChangeObject,
        to: ChangeObject,
    },
    Modified {
        before: ChangeObject,
        after: ChangeObject,
        fields: Vec<ChangedField>,
    },
    PartitionAdded {
        edge: PartitionEdge,
    },
    PartitionRemoved {
        edge: PartitionEdge,
    },
    PartitionModified {
        before: PartitionEdge,
        after: PartitionEdge,
    },
    DependencyAdded {
        edge: DependencyEdge,
    },
    DependencyRemoved {
        edge: DependencyEdge,
    },
}

/// A mapping supplied by the caller from a reviewed, trusted source.
#[derive(Debug, Clone, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RenameMapping {
    pub from: DatabaseObject,
    pub to: DatabaseObject,
}

#[derive(Debug, Clone, Eq, PartialEq, Serialize, Deserialize)]
pub struct SchemaDiffV1 {
    schema_version: SchemaVersionV1,
    pub before: DiffSource,
    pub after: DiffSource,
    pub same_requested_scope: bool,
    pub schemas: Vec<ComparedSchema>,
    pub coverage: CoverageStatus,
    pub changes: Vec<StructuralChange>,
    pub gaps: Vec<DiffGap>,
}

#[derive(Debug, Clone, Eq, PartialEq, Serialize, Deserialize)]
pub struct SchemaComparisonV1 {
    schema_version: SchemaVersionV1,
    pub expected_to_current: SchemaDiffV1,
    pub current_to_target: SchemaDiffV1,
}

#[derive(Debug, Error)]
pub enum DiffError {
    #[error(
        "rename mapping {index} must connect distinct qualified objects of the same supported kind and schema"
    )]
    InvalidRename { index: usize },
    #[error("rename mapping {index} repeats a source or destination")]
    ConflictingRename { index: usize },
    #[error(
        "rename mapping {index} needs observed endpoints and confirmed absence of each original name on the opposite side"
    )]
    UnverifiedRename { index: usize },
    #[error("rename mapping {index} changes the owning table or catalog kind")]
    RenameOwnerOrKind { index: usize },
    #[error("could not compute structural snapshot digest")]
    Digest(#[from] serde_json::Error),
}

pub fn compare_schemas<'a>(
    expected: impl Into<SchemaInput<'a>>,
    current: impl Into<SchemaInput<'a>>,
    target: impl Into<SchemaInput<'a>>,
    drift_renames: &[RenameMapping],
    proposed_renames: &[RenameMapping],
) -> Result<SchemaComparisonV1, DiffError> {
    let current = current.into();
    Ok(SchemaComparisonV1 {
        schema_version: SchemaVersionV1,
        expected_to_current: compare_snapshots(expected, current, drift_renames)?,
        current_to_target: compare_snapshots(current, target, proposed_renames)?,
    })
}

pub fn compare_snapshots<'a>(
    before: impl Into<SchemaInput<'a>>,
    after: impl Into<SchemaInput<'a>>,
    mappings: &[RenameMapping],
) -> Result<SchemaDiffV1, DiffError> {
    let before = Inventory::new(before.into());
    let after = Inventory::new(after.into());
    let renames = Renames::new(mappings, &before, &after)?;

    let schemas: Vec<_> = before
        .schemas()
        .chain(after.schemas())
        .collect::<BTreeSet<_>>()
        .into_iter()
        .map(|schema| ComparedSchema {
            schema: schema.into(),
            before: before.input.scope_state(schema),
            after: after.input.scope_state(schema),
        })
        .collect();
    let same_requested_scope = schemas
        .iter()
        .all(|schema| ![schema.before, schema.after].contains(&ScopeState::OutsideScope));

    let mut changes = schema_changes(&before, &after, &schemas);
    let mut gaps = before.report_gaps(DiffSide::Before);
    if schemas.is_empty() {
        gaps.push(DiffGap::EmptyScope);
    }
    gaps.extend(after.report_gaps(DiffSide::After));
    compare_objects(&before, &after, &renames, &mut changes, &mut gaps);
    compare_partitions(&before, &after, &renames, &mut changes, &mut gaps);
    compare_dependencies(&before, &after, &renames, &mut changes, &mut gaps);
    gaps.sort_unstable();
    gaps.dedup();
    let incomplete = !same_requested_scope
        || schemas.iter().any(|schema| {
            [schema.before, schema.after]
                .iter()
                .any(|state| matches!(state, ScopeState::Denied | ScopeState::Unavailable))
        })
        || gaps.iter().any(|gap| match gap {
            DiffGap::Inspection { side, gap } => {
                let input = match side {
                    DiffSide::Before => before.input,
                    DiffSide::After => after.input,
                };
                !input::is_verified_missing_schema(input, gap)
            }
            _ => true,
        });

    Ok(SchemaDiffV1 {
        schema_version: SchemaVersionV1,
        before: before.source()?,
        after: after.source()?,
        same_requested_scope,
        schemas,
        coverage: if incomplete {
            CoverageStatus::Incomplete
        } else {
            CoverageStatus::Complete
        },
        changes,
        gaps,
    })
}

fn schema_changes(
    before: &Inventory<'_>,
    after: &Inventory<'_>,
    schemas: &[ComparedSchema],
) -> Vec<StructuralChange> {
    schemas
        .iter()
        .filter_map(|scope| {
            let object = DatabaseObject {
                kind: ObjectKind::Schema,
                schema: None,
                name: scope.schema.clone(),
                member: None,
            };
            match (scope.before, scope.after) {
                (ScopeState::Visible, ScopeState::Missing)
                    if after.schema_is_missing(&scope.schema) =>
                {
                    Some(StructuralChange::Removed {
                        object: ChangeObject {
                            identity: object,
                            owning_table: None,
                        },
                    })
                }
                (ScopeState::Missing, ScopeState::Visible)
                    if before.schema_is_missing(&scope.schema) =>
                {
                    Some(StructuralChange::Added {
                        object: ChangeObject {
                            identity: object,
                            owning_table: None,
                        },
                    })
                }
                _ => None,
            }
        })
        .collect()
}

fn compare_objects(
    before: &Inventory<'_>,
    after: &Inventory<'_>,
    renames: &Renames<'_>,
    changes: &mut Vec<StructuralChange>,
    gaps: &mut Vec<DiffGap>,
) {
    let mut matched = BTreeSet::new();
    for (identity, left) in &before.objects {
        let destination = renames.forward(identity);
        let right = after
            .objects
            .get_key_value(destination.as_ref())
            .filter(|(_, right)| renames.owner_matches(*left, **right));
        if let Some((right_id, right)) = right {
            matched.insert(right_id);
            let before_object = left.change_object(identity);
            let after_object = right.change_object(right_id);
            if renames.is_explicit(identity) {
                changes.push(StructuralChange::Renamed {
                    from: before_object.clone(),
                    to: after_object.clone(),
                });
            }
            let fields = fields::compare(
                (*left, &before_object, before),
                (*right, &after_object, after),
                renames,
                gaps,
            );
            if !fields.is_empty() {
                changes.push(StructuralChange::Modified {
                    before: before_object,
                    after: after_object,
                    fields,
                });
            }
        } else {
            one_sided(
                (before, after),
                (*left, identity),
                (&destination, renames.owner_forward(left.owner())),
                DiffSide::After,
                changes,
                gaps,
            );
        }
    }

    for (identity, right) in &after.objects {
        if !matched.contains(identity) {
            let source = renames.backward(identity);
            one_sided(
                (after, before),
                (*right, identity),
                (&source, renames.owner_backward(right.owner())),
                DiffSide::Before,
                changes,
                gaps,
            );
        }
    }
}

fn one_sided(
    (present, other): (&Inventory<'_>, &Inventory<'_>),
    (record, identity): (Record<'_>, &DatabaseObject),
    (other_identity, other_owner): (&DatabaseObject, Option<&QualifiedName>),
    missing_from: DiffSide,
    changes: &mut Vec<StructuralChange>,
    gaps: &mut Vec<DiffGap>,
) {
    let object = record.change_object(identity);
    let present_side = match missing_from {
        DiffSide::Before => DiffSide::After,
        DiffSide::After => DiffSide::Before,
    };
    fields::record_unavailable(record, &object, present, present_side, gaps);
    if other_owner.is_some_and(|owner| !other.contains_table(owner)) {
        // The owning table's addition, removal or visibility gap covers its members.
        return;
    }
    if other.proves_absence(other_identity, other_owner) {
        changes.push(match missing_from {
            DiffSide::Before => StructuralChange::Added { object },
            DiffSide::After => StructuralChange::Removed { object },
        });
    } else {
        gaps.push(DiffGap::PresenceUnconfirmed {
            missing_from,
            object,
        });
    }
}

fn compare_partitions(
    before: &Inventory<'_>,
    after: &Inventory<'_>,
    renames: &Renames<'_>,
    changes: &mut Vec<StructuralChange>,
    gaps: &mut Vec<DiffGap>,
) {
    let after_partitions: BTreeMap<_, _> = after
        .partitions()
        .iter()
        .map(|edge| ((&edge.child, &edge.parent), edge))
        .collect();
    let mut matched = BTreeSet::new();
    for edge in before.partitions() {
        let child = renames.table_forward(&edge.child);
        let parent = renames.table_forward(&edge.parent);
        if let Some(current) = after_partitions.get(&(child, parent)) {
            matched.insert(*current);
            if edge.detach_pending != current.detach_pending {
                changes.push(StructuralChange::PartitionModified {
                    before: edge.clone(),
                    after: (*current).clone(),
                });
            }
        } else if after.knows_partition_absence(child, parent) {
            changes.push(StructuralChange::PartitionRemoved { edge: edge.clone() });
        } else {
            gaps.push(DiffGap::PartitionPresenceUnconfirmed {
                missing_from: DiffSide::After,
                edge: edge.clone(),
            });
        }
    }
    for edge in after.partitions() {
        if matched.contains(edge) {
            continue;
        }
        if before.knows_partition_absence(
            renames.table_backward(&edge.child),
            renames.table_backward(&edge.parent),
        ) {
            changes.push(StructuralChange::PartitionAdded { edge: edge.clone() });
        } else {
            gaps.push(DiffGap::PartitionPresenceUnconfirmed {
                missing_from: DiffSide::Before,
                edge: edge.clone(),
            });
        }
    }
}

fn compare_dependencies(
    before: &Inventory<'_>,
    after: &Inventory<'_>,
    renames: &Renames<'_>,
    changes: &mut Vec<StructuralChange>,
    gaps: &mut Vec<DiffGap>,
) {
    let mut matched = BTreeSet::new();
    for edge in before.dependencies() {
        let aligned = DependencyEdge {
            dependent: renames.forward(&edge.dependent).into_owned(),
            referenced: renames.forward(&edge.referenced).into_owned(),
            kind: edge.kind,
        };
        if let Ok(index) = after.dependencies().binary_search(&aligned) {
            matched.insert(index);
        } else if after.knows_dependency_absence(&aligned, before.is_foreign_key_edge(edge)) {
            changes.push(StructuralChange::DependencyRemoved { edge: edge.clone() });
        } else {
            gaps.push(DiffGap::DependencyPresenceUnconfirmed {
                missing_from: DiffSide::After,
                edge: edge.clone(),
            });
        }
    }
    for (index, edge) in after.dependencies().iter().enumerate() {
        if matched.contains(&index) {
            continue;
        }
        let aligned = DependencyEdge {
            dependent: renames.backward(&edge.dependent).into_owned(),
            referenced: renames.backward(&edge.referenced).into_owned(),
            kind: edge.kind,
        };
        if before.knows_dependency_absence(&aligned, after.is_foreign_key_edge(edge)) {
            changes.push(StructuralChange::DependencyAdded { edge: edge.clone() });
        } else {
            gaps.push(DiffGap::DependencyPresenceUnconfirmed {
                missing_from: DiffSide::Before,
                edge: edge.clone(),
            });
        }
    }
}
