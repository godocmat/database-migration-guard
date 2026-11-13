use dmg_core::{
    inspection::{
        CatalogField, InspectionField, InspectionGap, InspectionGapReason, InspectionGapScope,
        RelationEstimates,
    },
    risk::{DatabaseObject, ObjectKind},
    snapshot::{
        ColumnGeneration, ColumnIdentity, ColumnSnapshot, ConstraintKind, ConstraintSnapshot,
        DependencyEdge, DependencyKind, ForeignKeyTarget, IndexKey, IndexSnapshot, PartitionEdge,
        QualifiedName, TableKind, TableSnapshot,
    },
};
use tokio_postgres::{Row, Transaction, types::Type};

use crate::{ServerInfo, TransportError};

use super::{
    CatalogValue, Collection, Oid,
    details::{DefinitionKind, DefinitionRead},
    schema_gap, target_gap, value,
};

const RELATIONS: &str = include_str!("sql/relations.sql");
const UNSUPPORTED_OBJECTS: &str = include_str!("sql/unsupported_objects.sql");
const COLUMNS: &str = include_str!("sql/columns.sql");
const INDEXES: &str = include_str!("sql/indexes.sql");
const CONSTRAINTS: &str = include_str!("sql/constraints.sql");
const PARTITIONS: &str = include_str!("sql/partitions.sql");

pub(super) const PROJECTIONS: [InspectionField; 6] = [
    InspectionField::Relations,
    InspectionField::UnsupportedObjects,
    InspectionField::Columns,
    InspectionField::Indexes,
    InspectionField::Constraints,
    InspectionField::Partitions,
];

pub(super) struct Relation {
    pub(super) table: TableSnapshot,
    pub(super) estimates: RelationEstimates,
}

impl Collection {
    pub(super) async fn collect_structure(
        &mut self,
        tx: &mut Transaction<'_>,
        row_limit: u16,
        server: ServerInfo,
    ) -> Result<(), TransportError> {
        use InspectionField as Field;

        let limit = usize::from(row_limit);
        let sentinel = i64::from(row_limit) + 1;
        let namespaces: Vec<_> = self.namespaces.keys().copied().collect();

        for row in self
            .projection(
                tx,
                RELATIONS,
                &[(&namespaces, Type::OID_ARRAY), (&sentinel, Type::INT8)],
                Field::Relations,
                limit,
            )
            .await?
        {
            if let Err(reason) = self.record_relation(&row, server.block_size) {
                let scope = value::<Oid>(&row, 1)
                    .ok()
                    .and_then(|oid| self.namespaces.get(&oid));
                let gap = scope.map_or_else(
                    || target_gap(Field::Relations, reason),
                    |schema| schema_gap(schema, Field::Relations, reason),
                );
                self.structural_gaps.push(gap);
            }
        }
        for row in self
            .projection(
                tx,
                UNSUPPORTED_OBJECTS,
                &[(&namespaces, Type::OID_ARRAY)],
                Field::UnsupportedObjects,
                namespaces.len(),
            )
            .await?
        {
            let schema = value::<Oid>(&row, 0)
                .ok()
                .and_then(|oid| self.namespaces.get(&oid));
            self.structural_gaps.push(schema.map_or_else(
                || target_gap(Field::UnsupportedObjects, InspectionGapReason::InvalidValue),
                |schema| {
                    schema_gap(
                        schema,
                        Field::UnsupportedObjects,
                        InspectionGapReason::Unsupported,
                    )
                },
            ));
        }
        if self.relation_order.is_empty() {
            self.pending_projections.clear();
            return Ok(());
        }

        let relations = self.relation_order.clone();
        let relation_params = [
            (&relations as _, Type::OID_ARRAY),
            (&sentinel as _, Type::INT8),
        ];
        for row in self
            .projection(tx, COLUMNS, &relation_params, Field::Columns, limit)
            .await?
        {
            if let Err(reason) = self.record_column(&row) {
                self.record_row_gap(&row, Field::Columns, reason);
            }
        }
        for row in self
            .projection(tx, INDEXES, &relation_params, Field::Indexes, limit)
            .await?
        {
            if let Err(reason) = self.record_index(&row) {
                self.record_row_gap(&row, Field::Indexes, reason);
            }
        }
        let constraints = if server.server_version_num / 10_000 == 18 {
            CONSTRAINTS
                .replace("@ENFORCED@", "k.conenforced")
                .replace("@PERIOD@", "k.conperiod")
        } else {
            CONSTRAINTS
                .replace("@ENFORCED@", "true")
                .replace("@PERIOD@", "false")
        };
        let constraint_params = [
            (&relations as _, Type::OID_ARRAY),
            (&sentinel as _, Type::INT8),
            (&namespaces as _, Type::OID_ARRAY),
        ];
        for row in self
            .projection(
                tx,
                &constraints,
                &constraint_params,
                Field::Constraints,
                limit,
            )
            .await?
        {
            if let Err(reason) = self.record_constraint(&row) {
                self.record_row_gap(&row, Field::Constraints, reason);
            }
        }
        let partition_params = [
            (&relations as _, Type::OID_ARRAY),
            (&namespaces as _, Type::OID_ARRAY),
            (&sentinel as _, Type::INT8),
        ];
        for row in self
            .projection(tx, PARTITIONS, &partition_params, Field::Partitions, limit)
            .await?
        {
            if let Err(reason) = self.record_partition(&row) {
                self.structural_gaps
                    .push(target_gap(Field::Partitions, reason));
            }
        }
        Ok(())
    }

    fn record_relation(&mut self, row: &Row, block_size: u32) -> CatalogValue<()> {
        let oid = value(row, 0)?;
        let namespace = value(row, 1)?;
        let schema = self
            .namespaces
            .get(&namespace)
            .ok_or(InspectionGapReason::InvalidValue)?;
        let name = QualifiedName {
            schema: schema.clone(),
            name: identifier(row, 2)?,
        };
        let kind = match value::<i8>(row, 3)? as u8 {
            b'r' => TableKind::Ordinary,
            b'p' => TableKind::Partitioned,
            _ => return Err(InspectionGapReason::Unsupported),
        };
        let is_partition = value(row, 4)?;
        let estimates = if kind == TableKind::Ordinary {
            relation_estimates(
                &name,
                value(row, 5)?,
                value(row, 6)?,
                block_size,
                &mut self.structural_gaps,
            )
        } else {
            RelationEstimates {
                relation: name.clone(),
                rows: CatalogField::Unavailable,
                heap_bytes: CatalogField::NotApplicable,
                total_bytes: CatalogField::Unavailable,
            }
        };
        if is_partition || kind == TableKind::Partitioned {
            self.structural_gaps.push(relation_gap(
                &name,
                InspectionField::PartitionBound,
                InspectionGapReason::Unsupported,
            ));
        }
        if kind == TableKind::Ordinary {
            self.queue_size(oid);
        }
        self.relation_order.push(oid);
        self.relations.insert(
            oid,
            Relation {
                table: TableSnapshot {
                    name,
                    kind,
                    is_partition,
                    columns: Vec::new(),
                    indexes: Vec::new(),
                    constraints: Vec::new(),
                },
                estimates,
            },
        );
        Ok(())
    }

    fn record_column(&mut self, row: &Row) -> CatalogValue<()> {
        let table_oid = value(row, 0)?;
        let name = identifier(row, 1)?;
        let position = u16::try_from(value::<i16>(row, 2)?)
            .ok()
            .filter(|position| *position > 0)
            .ok_or(InspectionGapReason::InvalidValue)?;
        let array_dimensions =
            u16::try_from(value::<i32>(row, 6)?).map_err(|_| InspectionGapReason::InvalidValue)?;
        let identity = match value::<i8>(row, 8)? as u8 {
            0 => None,
            b'a' => Some(ColumnIdentity::Always),
            b'd' => Some(ColumnIdentity::ByDefault),
            _ => return Err(InspectionGapReason::Unsupported),
        };
        let generation = match value::<i8>(row, 9)? as u8 {
            0 => None,
            b's' => Some(ColumnGeneration::Stored),
            b'v' => Some(ColumnGeneration::Virtual),
            _ => return Err(InspectionGapReason::Unsupported),
        };
        let has_default = value(row, 10)?;
        let definition_oid: Option<Oid> = value(row, 11)?;
        let relation = self
            .relations
            .get_mut(&table_oid)
            .ok_or(InspectionGapReason::InvalidValue)?;
        let position_in_snapshot = relation.table.columns.len();
        relation.table.columns.push(ColumnSnapshot {
            name: name.clone(),
            position,
            data_type: QualifiedName {
                schema: identifier(row, 3)?,
                name: identifier(row, 4)?,
            },
            type_modifier: value(row, 5)?,
            array_dimensions,
            not_null: value(row, 7)?,
            identity,
            generation,
            has_default,
            default_definition: if has_default {
                CatalogField::Unavailable
            } else {
                CatalogField::NotApplicable
            },
        });
        match (has_default, definition_oid) {
            (true, Some(oid)) => self.queue_definition(DefinitionRead {
                relation: table_oid,
                oid,
                kind: DefinitionKind::Default(position_in_snapshot),
            }),
            (true, None) | (false, Some(_)) => self.structural_gaps.push(object_gap(
                column_object(&relation.table.name, &name),
                InspectionField::DefaultDefinition,
                InspectionGapReason::InvalidValue,
            )),
            (false, None) => {}
        }
        Ok(())
    }

    fn record_index(&mut self, row: &Row) -> CatalogValue<()> {
        let table_oid = value(row, 0)?;
        let oid = value(row, 1)?;
        let name = identifier(row, 2)?;
        let key_count = usize::try_from(value::<i16>(row, 10)?)
            .map_err(|_| InspectionGapReason::InvalidValue)?;
        let attributes =
            value::<Option<Vec<i16>>>(row, 11)?.ok_or(InspectionGapReason::Truncated)?;
        let relation = self
            .relations
            .get_mut(&table_oid)
            .ok_or(InspectionGapReason::InvalidValue)?;
        let (key_attributes, included_attributes) = attributes
            .split_at_checked(key_count)
            .filter(|(keys, _)| !keys.is_empty())
            .ok_or(InspectionGapReason::InvalidValue)?;
        let keys = key_attributes
            .iter()
            .map(|&attribute| {
                if attribute == 0 {
                    return Ok(IndexKey::Expression);
                }
                Ok(IndexKey::Column {
                    name: column_name(&relation.table, attribute)?,
                })
            })
            .collect::<CatalogValue<Vec<_>>>()?;
        let included_columns = column_names(&relation.table, included_attributes)?;
        let position_in_snapshot = relation.table.indexes.len();
        relation.table.indexes.push(IndexSnapshot {
            name,
            access_method: identifier(row, 3)?,
            unique: value(row, 4)?,
            primary: value(row, 5)?,
            valid: value(row, 6)?,
            ready: value(row, 7)?,
            nulls_not_distinct: value(row, 8)?,
            partial: value(row, 9)?,
            keys,
            included_columns,
            definition: CatalogField::Unavailable,
        });
        self.queue_definition(DefinitionRead {
            relation: table_oid,
            oid,
            kind: DefinitionKind::Index(position_in_snapshot),
        });
        Ok(())
    }

    fn record_constraint(&mut self, row: &Row) -> CatalogValue<()> {
        let table_oid = value(row, 0)?;
        let oid = value(row, 1)?;
        let name = identifier(row, 2)?;
        let kind = match value::<i8>(row, 3)? as u8 {
            b'c' => ConstraintKind::Check,
            b'n' => ConstraintKind::NotNull,
            b'p' => ConstraintKind::PrimaryKey,
            b'u' => ConstraintKind::Unique,
            b'f' => ConstraintKind::ForeignKey,
            b'x' => ConstraintKind::Exclusion,
            _ => return Err(InspectionGapReason::Unsupported),
        };
        let table = &self
            .relations
            .get(&table_oid)
            .ok_or(InspectionGapReason::InvalidValue)?
            .table;
        let object = constraint_object(&table.name, &name);
        let columns = constraint_columns(row, table, 8, 13)?;
        let references = if kind == ConstraintKind::ForeignKey {
            Some(self.foreign_key_target(row, &columns, &object)?)
        } else {
            None
        };
        let period: bool = value(row, 12)?;
        let definition_blocked = match &references {
            Some(ForeignKeyTarget::OutsideScope) => Some(InspectionGapReason::OutsideScope),
            Some(ForeignKeyTarget::Unavailable) => Some(InspectionGapReason::Unsupported),
            _ => None,
        };
        if period {
            self.structural_gaps.push(object_gap(
                object.clone(),
                InspectionField::Constraints,
                InspectionGapReason::Unsupported,
            ));
        }
        let columns = columns.unwrap_or_else(|reason| {
            self.structural_gaps.push(object_gap(
                object.clone(),
                InspectionField::Constraints,
                reason,
            ));
            Vec::new()
        });
        let constraint = ConstraintSnapshot {
            name,
            kind,
            columns,
            validated: value(row, 4)?,
            enforced: value(row, 5)?,
            deferrable: value(row, 6)?,
            initially_deferred: value(row, 7)?,
            references,
            definition: CatalogField::Unavailable,
        };
        let relation = self
            .relations
            .get_mut(&table_oid)
            .ok_or(InspectionGapReason::InvalidValue)?;
        let position_in_snapshot = relation.table.constraints.len();
        relation.table.constraints.push(constraint);
        if let Some(reason) = definition_blocked {
            self.structural_gaps.push(object_gap(
                object,
                InspectionField::ConstraintDefinition,
                reason,
            ));
        } else {
            self.queue_definition(DefinitionRead {
                relation: table_oid,
                oid,
                kind: DefinitionKind::Constraint(position_in_snapshot),
            });
        }
        Ok(())
    }

    fn foreign_key_target(
        &mut self,
        row: &Row,
        columns: &CatalogValue<Vec<String>>,
        dependent: &DatabaseObject,
    ) -> CatalogValue<ForeignKeyTarget> {
        let target_visible: Option<bool> = value(row, 11)?;
        if target_visible == Some(false) {
            self.structural_gaps.push(object_gap(
                dependent.clone(),
                InspectionField::Dependencies,
                InspectionGapReason::OutsideScope,
            ));
            return Ok(ForeignKeyTarget::OutsideScope);
        }

        let target_oid: Option<Oid> = value(row, 10)?;
        let target = target_oid.and_then(|oid| self.relations.get(&oid));
        let Some(target) = target else {
            let reason = if target_visible == Some(true) {
                InspectionGapReason::Unsupported
            } else {
                InspectionGapReason::InvalidValue
            };
            self.structural_gaps.push(object_gap(
                dependent.clone(),
                InspectionField::Dependencies,
                reason,
            ));
            return Ok(ForeignKeyTarget::Unavailable);
        };
        self.dependencies.push(DependencyEdge {
            dependent: dependent.clone(),
            referenced: relation_object(&target.table.name),
            kind: DependencyKind::Normal,
        });

        let target_columns = constraint_columns(row, &target.table, 9, 14)?;
        match (columns, target_columns) {
            (Ok(columns), Ok(target_columns))
                if !columns.is_empty() && columns.len() == target_columns.len() =>
            {
                Ok(ForeignKeyTarget::Observed {
                    table: target.table.name.clone(),
                    columns: target_columns,
                })
            }
            (columns, target_columns) => {
                let reason = columns
                    .as_ref()
                    .err()
                    .copied()
                    .or_else(|| target_columns.err())
                    .unwrap_or(InspectionGapReason::InvalidValue);
                self.structural_gaps.push(object_gap(
                    dependent.clone(),
                    InspectionField::Constraints,
                    reason,
                ));
                Ok(ForeignKeyTarget::Unavailable)
            }
        }
    }

    fn record_partition(&mut self, row: &Row) -> CatalogValue<()> {
        let child_oid: Option<Oid> = value(row, 0)?;
        let parent_oid: Option<Oid> = value(row, 1)?;
        let child = child_oid.and_then(|oid| self.relations.get(&oid));
        let parent = parent_oid.and_then(|oid| self.relations.get(&oid));
        let is_partition: bool = value(row, 2)?;
        let detach_pending = value(row, 3)?;
        if let (Some(child), Some(parent), true) = (child, parent, is_partition) {
            self.partition_edges.push(PartitionEdge {
                child: child.table.name.clone(),
                parent: parent.table.name.clone(),
                detach_pending,
            });
        } else if let Some(observed) = child.or(parent) {
            let reason = if !is_partition {
                InspectionGapReason::Unsupported
            } else if child_oid.is_none() || parent_oid.is_none() {
                InspectionGapReason::OutsideScope
            } else {
                InspectionGapReason::Unsupported
            };
            self.structural_gaps.push(relation_gap(
                &observed.table.name,
                InspectionField::Partitions,
                reason,
            ));
        } else {
            return Err(InspectionGapReason::InvalidValue);
        }
        Ok(())
    }

    fn record_row_gap(&mut self, row: &Row, field: InspectionField, reason: InspectionGapReason) {
        let table = value::<Oid>(row, 0)
            .ok()
            .and_then(|oid| self.relations.get(&oid));
        let gap = table.map_or_else(
            || target_gap(field, reason),
            |relation| relation_gap(&relation.table.name, field, reason),
        );
        self.structural_gaps.push(gap);
    }
}

fn relation_estimates(
    relation: &QualifiedName,
    reltuples: f32,
    relpages: i32,
    block_size: u32,
    gaps: &mut Vec<InspectionGap>,
) -> RelationEstimates {
    let rounded = f64::from(reltuples).round();
    // The f64 representation of u64::MAX rounds up to the exclusive upper bound.
    let rows = if reltuples >= 0.0 && rounded < u64::MAX as f64 {
        CatalogField::Estimated {
            value: rounded as u64,
        }
    } else {
        let reason = if reltuples == -1.0 {
            InspectionGapReason::Missing
        } else {
            InspectionGapReason::InvalidValue
        };
        gaps.push(relation_gap(relation, InspectionField::PlannerRows, reason));
        CatalogField::Unavailable
    };
    let heap_bytes = match u64::try_from(relpages) {
        Ok(pages) if pages > 0 || matches!(rows, CatalogField::Estimated { .. }) => {
            CatalogField::Estimated {
                value: pages * u64::from(block_size),
            }
        }
        pages => {
            let reason = if pages.is_ok() {
                InspectionGapReason::Missing
            } else {
                InspectionGapReason::InvalidValue
            };
            gaps.push(relation_gap(
                relation,
                InspectionField::EstimatedHeapBytes,
                reason,
            ));
            CatalogField::Unavailable
        }
    };
    RelationEstimates {
        relation: relation.clone(),
        rows,
        heap_bytes,
        total_bytes: CatalogField::Unavailable,
    }
}

fn constraint_columns(
    row: &Row,
    table: &TableSnapshot,
    attributes_index: usize,
    truncated_index: usize,
) -> CatalogValue<CatalogValue<Vec<String>>> {
    if value::<bool>(row, truncated_index)? {
        return Ok(Err(InspectionGapReason::Truncated));
    }
    let attributes: Option<Vec<i16>> = value(row, attributes_index)?;
    Ok(column_names(
        table,
        attributes.as_deref().unwrap_or_default(),
    ))
}

fn column_names(table: &TableSnapshot, attributes: &[i16]) -> CatalogValue<Vec<String>> {
    attributes
        .iter()
        .map(|&attribute| column_name(table, attribute))
        .collect()
}

fn column_name(table: &TableSnapshot, attribute: i16) -> CatalogValue<String> {
    let position = u16::try_from(attribute)
        .ok()
        .filter(|position| *position > 0)
        .ok_or(InspectionGapReason::Unsupported)?;
    let index = table
        .columns
        .binary_search_by_key(&position, |column| column.position)
        .map_err(|_| InspectionGapReason::InvalidValue)?;
    Ok(table.columns[index].name.clone())
}

fn identifier(row: &Row, index: usize) -> CatalogValue<String> {
    let name: String = value(row, index)?;
    if name.is_empty() || name.len() > 63 || name.chars().any(char::is_control) {
        return Err(InspectionGapReason::InvalidValue);
    }
    Ok(name)
}

pub(super) fn object_gap(
    object: DatabaseObject,
    field: InspectionField,
    reason: InspectionGapReason,
) -> InspectionGap {
    InspectionGap {
        scope: InspectionGapScope::Object { object },
        field,
        reason,
    }
}

pub(super) fn relation_gap(
    relation: &QualifiedName,
    field: InspectionField,
    reason: InspectionGapReason,
) -> InspectionGap {
    object_gap(relation_object(relation), field, reason)
}

fn relation_object(relation: &QualifiedName) -> DatabaseObject {
    DatabaseObject {
        kind: ObjectKind::Relation,
        schema: Some(relation.schema.clone()),
        name: relation.name.clone(),
        member: None,
    }
}

pub(super) fn column_object(relation: &QualifiedName, column: &str) -> DatabaseObject {
    DatabaseObject {
        kind: ObjectKind::Column,
        schema: Some(relation.schema.clone()),
        name: relation.name.clone(),
        member: Some(column.into()),
    }
}

pub(super) fn constraint_object(relation: &QualifiedName, constraint: &str) -> DatabaseObject {
    DatabaseObject {
        kind: ObjectKind::Constraint,
        schema: Some(relation.schema.clone()),
        name: relation.name.clone(),
        member: Some(constraint.into()),
    }
}

pub(super) fn index_object(relation: &QualifiedName, index: &str) -> DatabaseObject {
    DatabaseObject {
        kind: ObjectKind::Index,
        schema: Some(relation.schema.clone()),
        name: index.into(),
        member: None,
    }
}
