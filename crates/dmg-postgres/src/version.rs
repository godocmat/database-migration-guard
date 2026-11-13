use std::collections::BTreeMap;

use dmg_core::{
    coverage::{CoverageGap, CoverageGapKind},
    risk::ObjectKind,
    target::PostgresTarget,
};

use crate::{
    AlterChange, ConstraintKind, Operation, RelationKind, RelationName, Statement, Transaction,
    parse::span_gap,
};

#[derive(Default)]
pub(crate) struct VersionContext {
    relations: BTreeMap<RelationName, RelationKind>,
}

impl VersionContext {
    pub(crate) fn clear(&mut self) {
        self.relations.clear();
    }

    pub(crate) fn check(
        &mut self,
        target: PostgresTarget,
        statements: &[Statement],
        gaps: &mut Vec<CoverageGap>,
    ) {
        if target != PostgresTarget::V17 {
            return;
        }
        for statement in statements {
            match &statement.operation {
                Operation::CreateTable(table) => {
                    self.relations.remove(&table.relation);
                    if table.relation.schema.is_some() && !table.if_not_exists {
                        self.relations.insert(table.relation.clone(), table.kind);
                    }
                }
                Operation::AlterTable(table) => {
                    for action in &table.actions {
                        let AlterChange::AddConstraint(constraint) = &action.change else {
                            continue;
                        };
                        let issue = match constraint.kind {
                            ConstraintKind::NotNull => Some((
                                CoverageGapKind::IncompatibleTarget,
                                "table-level NOT NULL constraints require PostgreSQL 18",
                            )),
                            ConstraintKind::ForeignKey { .. } if constraint.not_valid => {
                                match self.relations.get(&table.relation) {
                                    Some(RelationKind::PartitionedTable) => Some((
                                        CoverageGapKind::IncompatibleTarget,
                                        "NOT VALID foreign keys on partitioned tables require PostgreSQL 18",
                                    )),
                                    Some(_) => None,
                                    None => Some((
                                        CoverageGapKind::UnresolvedDependency,
                                        "table kind is needed to check a NOT VALID foreign key on PostgreSQL 17",
                                    )),
                                }
                            }
                            ConstraintKind::Check { .. } if !constraint.enforced => Some((
                                CoverageGapKind::IncompatibleTarget,
                                "NOT ENFORCED constraints require PostgreSQL 18",
                            )),
                            _ => None,
                        };
                        if let Some((kind, message)) = issue {
                            gaps.push(span_gap(action.span.clone(), kind, message));
                        }
                    }
                }
                Operation::Rename(rename) if rename.kind == ObjectKind::Relation => {
                    if let Some(kind) = self.relations.remove(&rename.object) {
                        let mut new_name = rename.object.clone();
                        new_name.name.clone_from(&rename.new_name);
                        self.relations.insert(new_name, kind);
                    } else {
                        self.clear();
                    }
                }
                Operation::Drop(drop)
                    if drop.kind == ObjectKind::Relation
                        && !drop.cascade
                        && drop.objects.iter().all(|name| name.schema.is_some()) =>
                {
                    // A drop of a partitioned parent may remove child tables as well.
                    if drop.objects.iter().any(|name| {
                        self.relations.get(name) == Some(&RelationKind::PartitionedTable)
                    }) {
                        self.clear();
                    } else {
                        for name in &drop.objects {
                            self.relations.remove(name);
                        }
                    }
                }
                Operation::Drop(drop)
                    if drop.kind == ObjectKind::Relation || drop.kind == ObjectKind::Schema =>
                {
                    self.clear()
                }
                Operation::Transaction(
                    Transaction::Rollback { .. } | Transaction::RollbackTo(_),
                )
                | Operation::Unsupported => self.clear(),
                _ => {}
            }
        }
    }
}
