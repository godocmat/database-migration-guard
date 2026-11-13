use crate::{
    inspection::CatalogField,
    snapshot::{ForeignKeyTarget, IndexKey},
};

use super::{input::Record, *};

pub(super) fn compare(
    (left, before, before_inventory): (Record<'_>, &ChangeObject, &Inventory<'_>),
    (right, after, after_inventory): (Record<'_>, &ChangeObject, &Inventory<'_>),
    renames: &Renames<'_>,
    gaps: &mut Vec<DiffGap>,
) -> Vec<ChangedField> {
    let mut comparison = FieldComparison {
        before_inventory,
        after_inventory,
        before,
        after,
        fields: Vec::new(),
        gaps,
    };
    match (left, right) {
        (Record::Table(left), Record::Table(right)) => {
            comparison.value(ChangedField::TableKind, &left.kind, &right.kind);
            comparison.value(
                ChangedField::PartitionMembership,
                &left.is_partition,
                &right.is_partition,
            );
        }
        (Record::Column(_, left), Record::Column(_, right)) => {
            comparison.value(
                ChangedField::ColumnPosition,
                &left.position,
                &right.position,
            );
            comparison.value(ChangedField::DataType, &left.data_type, &right.data_type);
            comparison.value(
                ChangedField::TypeModifier,
                &left.type_modifier,
                &right.type_modifier,
            );
            comparison.value(
                ChangedField::ArrayDimensions,
                &left.array_dimensions,
                &right.array_dimensions,
            );
            comparison.value(ChangedField::Nullability, &left.not_null, &right.not_null);
            comparison.value(ChangedField::Identity, &left.identity, &right.identity);
            comparison.value(
                ChangedField::Generation,
                &left.generation,
                &right.generation,
            );
            comparison.value(
                ChangedField::DefaultPresence,
                &left.has_default,
                &right.has_default,
            );
            comparison.definition(
                ChangedField::DefaultDefinition,
                InspectionField::DefaultDefinition,
                &left.default_definition,
                &right.default_definition,
            );
        }
        (Record::Index(table, left), Record::Index(_, right)) => {
            comparison.value(
                ChangedField::AccessMethod,
                &left.access_method,
                &right.access_method,
            );
            comparison.value(ChangedField::Unique, &left.unique, &right.unique);
            comparison.value(ChangedField::Primary, &left.primary, &right.primary);
            comparison.value(ChangedField::Valid, &left.valid, &right.valid);
            comparison.value(ChangedField::Ready, &left.ready, &right.ready);
            comparison.value(
                ChangedField::NullsNotDistinct,
                &left.nulls_not_distinct,
                &right.nulls_not_distinct,
            );
            comparison.value(ChangedField::Partial, &left.partial, &right.partial);
            let same_keys = left.keys.len() == right.keys.len()
                && left
                    .keys
                    .iter()
                    .zip(&right.keys)
                    .all(|(old, new)| match (old, new) {
                        (IndexKey::Column { name: old }, IndexKey::Column { name: new }) => {
                            renames.column_matches(&table.name, old, new)
                        }
                        (IndexKey::Expression, IndexKey::Expression) => true,
                        _ => false,
                    });
            comparison.difference(ChangedField::IndexKeys, !same_keys);
            comparison.difference(
                ChangedField::IncludedColumns,
                !columns_match(
                    renames,
                    &table.name,
                    &left.included_columns,
                    &right.included_columns,
                ),
            );
            comparison.definition(
                ChangedField::IndexDefinition,
                InspectionField::IndexDefinition,
                &left.definition,
                &right.definition,
            );
        }
        (Record::Constraint(table, left), Record::Constraint(_, right)) => {
            comparison.value(ChangedField::ConstraintKind, &left.kind, &right.kind);
            let columns_known = comparison.field_available(
                ChangedField::ConstraintColumns,
                comparison
                    .before_inventory
                    .object_projection_gap(InspectionField::Constraints, before),
                comparison
                    .after_inventory
                    .object_projection_gap(InspectionField::Constraints, after),
            );
            if columns_known {
                comparison.difference(
                    ChangedField::ConstraintColumns,
                    !columns_match(renames, &table.name, &left.columns, &right.columns),
                );
            }
            comparison.value(ChangedField::Validated, &left.validated, &right.validated);
            comparison.value(ChangedField::Enforced, &left.enforced, &right.enforced);
            comparison.value(
                ChangedField::Deferrable,
                &left.deferrable,
                &right.deferrable,
            );
            comparison.value(
                ChangedField::InitiallyDeferred,
                &left.initially_deferred,
                &right.initially_deferred,
            );
            comparison.references(renames, left.references.as_ref(), right.references.as_ref());
            comparison.definition(
                ChangedField::ConstraintDefinition,
                InspectionField::ConstraintDefinition,
                &left.definition,
                &right.definition,
            );
        }
        _ => unreachable!("the diff pairs records by object kind"),
    }
    comparison.fields
}

pub(super) fn record_unavailable(
    record: Record<'_>,
    object: &ChangeObject,
    inventory: &Inventory<'_>,
    side: DiffSide,
    gaps: &mut Vec<DiffGap>,
) {
    let field = match record {
        Record::Table(_) => return,
        Record::Column(_, value) => (
            ChangedField::DefaultDefinition,
            InspectionField::DefaultDefinition,
            &value.default_definition,
        ),
        Record::Index(_, value) => (
            ChangedField::IndexDefinition,
            InspectionField::IndexDefinition,
            &value.definition,
        ),
        Record::Constraint(_, value) => {
            if inventory.object_projection_gap(InspectionField::Constraints, object) {
                gaps.push(DiffGap::FieldUnavailable {
                    side,
                    object: object.clone(),
                    field: ChangedField::ConstraintColumns,
                });
            }
            if reference_unavailable(value.references.as_ref()) {
                gaps.push(DiffGap::FieldUnavailable {
                    side,
                    object: object.clone(),
                    field: ChangedField::ConstraintReferences,
                });
            }
            (
                ChangedField::ConstraintDefinition,
                InspectionField::ConstraintDefinition,
                &value.definition,
            )
        }
    };
    if definition_unavailable(inventory, object, field.1, field.2) {
        gaps.push(DiffGap::FieldUnavailable {
            side,
            object: object.clone(),
            field: field.0,
        });
    }
}

fn columns_match(
    renames: &Renames<'_>,
    table: &QualifiedName,
    before: &[String],
    after: &[String],
) -> bool {
    before.len() == after.len()
        && before
            .iter()
            .zip(after)
            .all(|(left, right)| renames.column_matches(table, left, right))
}

fn reference_unavailable(value: Option<&ForeignKeyTarget>) -> bool {
    matches!(
        value,
        Some(ForeignKeyTarget::OutsideScope | ForeignKeyTarget::Unavailable)
    )
}

fn definition_unavailable(
    inventory: &Inventory<'_>,
    object: &ChangeObject,
    field: InspectionField,
    value: &CatalogField<String>,
) -> bool {
    match value {
        CatalogField::NotApplicable => false,
        CatalogField::Observed { .. } => inventory.object_projection_gap(field, object),
        CatalogField::Estimated { .. } | CatalogField::Unavailable => true,
    }
}

struct FieldComparison<'a, 'input> {
    before_inventory: &'a Inventory<'input>,
    after_inventory: &'a Inventory<'input>,
    before: &'a ChangeObject,
    after: &'a ChangeObject,
    fields: Vec<ChangedField>,
    gaps: &'a mut Vec<DiffGap>,
}

impl FieldComparison<'_, '_> {
    fn value<T: PartialEq>(&mut self, field: ChangedField, before: &T, after: &T) {
        self.difference(field, before != after);
    }

    fn difference(&mut self, field: ChangedField, changed: bool) {
        if changed {
            self.fields.push(field);
        }
    }

    fn field_available(
        &mut self,
        field: ChangedField,
        before_unavailable: bool,
        after_unavailable: bool,
    ) -> bool {
        for (side, object, unavailable) in [
            (DiffSide::Before, self.before, before_unavailable),
            (DiffSide::After, self.after, after_unavailable),
        ] {
            if unavailable {
                self.gaps.push(DiffGap::FieldUnavailable {
                    side,
                    object: object.clone(),
                    field,
                });
            }
        }
        !before_unavailable && !after_unavailable
    }

    fn definition(
        &mut self,
        field: ChangedField,
        inspection_field: InspectionField,
        before: &CatalogField<String>,
        after: &CatalogField<String>,
    ) {
        if self.field_available(
            field,
            definition_unavailable(self.before_inventory, self.before, inspection_field, before),
            definition_unavailable(self.after_inventory, self.after, inspection_field, after),
        ) {
            self.value(field, before, after);
        }
    }

    fn references(
        &mut self,
        renames: &Renames<'_>,
        before: Option<&ForeignKeyTarget>,
        after: Option<&ForeignKeyTarget>,
    ) {
        let field = ChangedField::ConstraintReferences;
        if self.field_available(
            field,
            reference_unavailable(before),
            reference_unavailable(after),
        ) {
            let changed = match (before, after) {
                (
                    Some(ForeignKeyTarget::Observed {
                        table: old_table,
                        columns: old_columns,
                    }),
                    Some(ForeignKeyTarget::Observed {
                        table: new_table,
                        columns: new_columns,
                    }),
                ) => {
                    renames.table_forward(old_table) != new_table
                        || !columns_match(renames, old_table, old_columns, new_columns)
                }
                _ => before != after,
            };
            self.difference(field, changed);
        }
    }
}
