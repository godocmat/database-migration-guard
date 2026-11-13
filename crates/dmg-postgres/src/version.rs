use std::collections::BTreeMap;

use dmg_core::{
    coverage::{CoverageGap, CoverageGapKind, DiagnosticScope},
    migration::TransactionWrapper,
    risk::ObjectKind,
    source::SourceSpan,
    target::PostgresTarget,
};

use crate::{
    AlterChange, Constraint, ConstraintKind, Operation, ParsedMigration, RelationKind,
    RelationName, Transaction, parse::span_gap,
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
        migration: &ParsedMigration,
        gaps: &mut Vec<CoverageGap>,
        syntax_complete: bool,
    ) {
        if !syntax_complete {
            self.clear();
        }
        let whole_file_unknown = gaps
            .iter()
            .any(|gap| !matches!(gap.scope, DiagnosticScope::Span { .. }));
        let mut source_spans: Vec<_> = gaps
            .iter()
            .filter_map(|gap| match &gap.scope {
                DiagnosticScope::Span { span } => Some(span.bytes),
                _ => None,
            })
            .collect();
        source_spans.sort_unstable();
        let mut next_gap = source_spans.into_iter().peekable();
        let mut explicit_transaction = false;
        for statement in &migration.statements {
            let mut supported = !whole_file_unknown;
            while next_gap
                .peek()
                .is_some_and(|gap| gap.start() < statement.span.bytes.end())
            {
                let gap = next_gap.next().expect("checked that a source gap exists");
                self.clear();
                supported &= gap.end() <= statement.span.bytes.start();
            }
            match &statement.operation {
                Operation::CreateTable(table) => {
                    let version_gap_start = gaps.len();
                    for constraint in &table.constraints {
                        check_constraint(
                            target,
                            constraint,
                            ConstraintSite::Table,
                            Some(table.kind),
                            &statement.span,
                            gaps,
                        );
                    }
                    for constraint in table.columns.iter().flat_map(|column| &column.constraints) {
                        check_constraint(
                            target,
                            constraint,
                            ConstraintSite::Column,
                            Some(table.kind),
                            &statement.span,
                            gaps,
                        );
                    }
                    let version_ok = gaps.len() == version_gap_start;
                    self.relations.remove(&table.relation);
                    if supported
                        && version_ok
                        && table.relation.schema.is_some()
                        && !table.if_not_exists
                    {
                        self.relations.insert(table.relation.clone(), table.kind);
                    }
                }
                Operation::AlterTable(table) => {
                    let kind = self.relations.get(&table.relation).copied();
                    for action in &table.actions {
                        if let AlterChange::AddConstraint(constraint) = &action.change {
                            check_constraint(
                                target,
                                constraint,
                                ConstraintSite::Alter,
                                kind,
                                &action.span,
                                gaps,
                            );
                        }
                    }
                    if !supported {
                        self.clear();
                    }
                }
                Operation::Rename(rename) if rename.kind == ObjectKind::Relation && supported => {
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
                    if matches!(drop.kind, ObjectKind::Relation | ObjectKind::Schema) =>
                {
                    self.clear()
                }
                Operation::CreateIndex(index)
                    if index.concurrently
                        && self.relations.get(&index.relation)
                            == Some(&RelationKind::PartitionedTable) =>
                {
                    gaps.push(span_gap(
                        statement.span.clone(),
                        CoverageGapKind::IncompatibleTarget,
                        "CREATE INDEX CONCURRENTLY is unavailable on partitioned parent tables",
                    ));
                }
                Operation::Transaction(Transaction::Begin) => explicit_transaction = true,
                Operation::Transaction(Transaction::Commit { chain }) => {
                    explicit_transaction = *chain
                }
                Operation::Transaction(Transaction::Rollback { chain }) => {
                    explicit_transaction = *chain;
                    self.clear();
                }
                Operation::Transaction(Transaction::RollbackTo(_)) | Operation::Unsupported => {
                    self.clear()
                }
                _ if !supported => self.clear(),
                _ => {}
            }
        }
        if !gaps.is_empty()
            || (explicit_transaction
                && migration.execution.transaction_wrapper == TransactionWrapper::None)
        {
            self.clear();
        }
    }
}

#[derive(Clone, Copy)]
enum ConstraintSite {
    Alter,
    Table,
    Column,
}

fn check_constraint(
    target: PostgresTarget,
    constraint: &Constraint,
    site: ConstraintSite,
    relation_kind: Option<RelationKind>,
    location: &SourceSpan,
    gaps: &mut Vec<CoverageGap>,
) {
    if target >= PostgresTarget::V18 {
        return;
    }
    let issue = match constraint.kind {
        ConstraintKind::Check { .. } | ConstraintKind::ForeignKey { .. }
            if !constraint.enforced =>
        {
            Some((
                CoverageGapKind::IncompatibleTarget,
                "NOT ENFORCED constraints require PostgreSQL 18",
            ))
        }
        ConstraintKind::NotNull
            if !matches!(site, ConstraintSite::Column)
                || constraint.no_inherit
                || constraint.not_valid =>
        {
            Some((
                CoverageGapKind::IncompatibleTarget,
                "table-level NOT NULL constraints require PostgreSQL 18",
            ))
        }
        ConstraintKind::ForeignKey { .. }
            if matches!(site, ConstraintSite::Alter) && constraint.not_valid =>
        {
            match relation_kind {
                Some(RelationKind::PartitionedTable) => Some((
                    CoverageGapKind::IncompatibleTarget,
                    "NOT VALID foreign keys on partitioned tables require PostgreSQL 18",
                )),
                Some(_) => None,
                None => Some((
                    CoverageGapKind::UnresolvedDependency,
                    "table kind is needed to check a NOT VALID foreign key before PostgreSQL 18",
                )),
            }
        }
        _ => None,
    };
    if let Some((kind, message)) = issue {
        gaps.push(span_gap(location.clone(), kind, message));
    }
}
