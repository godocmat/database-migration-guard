//! Static DDL risk findings and relation lock predictions.

#[cfg(test)]
mod tests;

use std::collections::BTreeSet;

use dmg_core::{
    coverage::{CoverageGap, CoverageGapKind, DiagnosticScope},
    risk::{Confidence, DatabaseObject, Finding, ObjectKind, Severity},
    source::SourceSpan,
    target::PostgresTarget,
};
use dmg_postgres::{
    AlterAction, AlterChange, AlterTable, ColumnDefinition, Constraint, ConstraintKind, DataType,
    DefaultExpr, DropObjects, Operation, ParsedMigration, ParsedPlan, RelationKind, RelationName,
    Rename, Statement,
};

use crate::{
    RelationLock, RelationLockObservation, Rule, RuleOptions,
    finding::{FindingSpec, Selection, record},
    schema::{RelationKey, Schema, TableFacts, resolve},
    transaction::{TransactionContext, TransactionEffect, TransactionTimeline},
};

pub(crate) fn evaluate(
    plan: &ParsedPlan,
    target: PostgresTarget,
    options: &RuleOptions,
    timelines: &[TransactionTimeline],
    findings: &mut Vec<Finding>,
    gaps: &mut Vec<CoverageGap>,
    locks: &mut Vec<RelationLockObservation>,
) {
    let selection = Selection::new(plan);
    let mut schema = Schema::default();
    for (migration, timeline) in plan.migrations.iter().zip(timelines) {
        let selected = selection.contains(&migration.path);
        for (statement, step) in migration.statements.iter().zip(&timeline.steps) {
            if step.context == TransactionContext::Unknown {
                schema.invalidate();
            }
            if step.starts_block {
                schema.start_block();
            }

            let incomplete = statement_has_gap(migration, statement);
            let incompatible = migration.coverage.gaps().iter().any(|gap| {
                gap.kind == CoverageGapKind::IncompatibleTarget && gap_overlaps(gap, statement)
            });
            if selected {
                Ddl {
                    migration,
                    target,
                    options,
                    schema: &schema,
                    finds: findings,
                    gaps,
                    locks,
                    complete_statement: !incomplete
                        && step.conflict.is_none()
                        && step.effect != TransactionEffect::Invalidate,
                    predictable_lock: !incompatible && step.conflict.is_none(),
                }
                .inspect(statement);
            }

            if incomplete || step.effect == TransactionEffect::Invalidate {
                schema.invalidate();
            } else {
                update_schema(&mut schema, &options.search_path, &statement.operation);
                schema.finish_step(step.effect);
            }
        }
        schema.finish_step(timeline.end_effect);
        if migration.coverage.gaps().iter().any(|gap| {
            matches!(
                gap.kind,
                CoverageGapKind::ParseError
                    | CoverageGapKind::UnreadableInput
                    | CoverageGapKind::InputLimitExceeded
                    | CoverageGapKind::UnsupportedInput
            )
        }) {
            // A missing last statement can occur after the final timeline step.
            schema.invalidate();
        }
    }
}

fn statement_has_gap(migration: &ParsedMigration, statement: &Statement) -> bool {
    migration
        .coverage
        .gaps()
        .iter()
        .any(|gap| gap_overlaps(gap, statement))
}

fn gap_overlaps(gap: &CoverageGap, statement: &Statement) -> bool {
    match &gap.scope {
        DiagnosticScope::Span { span } if span.bytes.is_empty() => {
            statement.span.bytes.start() <= span.bytes.start()
                && span.bytes.start() <= statement.span.bytes.end()
        }
        DiagnosticScope::Span { span } => {
            span.bytes.start() < statement.span.bytes.end()
                && statement.span.bytes.start() < span.bytes.end()
        }
        _ => true,
    }
}

fn update_schema(schema: &mut Schema, search_path: &[String], operation: &Operation) {
    let key = |relation| resolve(relation, search_path);
    match operation {
        Operation::CreateTable(table) => {
            if let Some(key) = key(&table.relation) {
                schema.create_table(key, table);
            } else {
                schema.invalidate();
            }
        }
        Operation::AlterTable(alter) => {
            if let Some(key) = key(&alter.relation)
                && !alter.actions.iter().any(|action| {
                    matches!(
                        action.change,
                        AlterChange::DropColumn { cascade: true, .. }
                            | AlterChange::DropConstraint { cascade: true, .. }
                    )
                })
            {
                schema.alter_table(key, alter);
            } else {
                schema.invalidate();
            }
        }
        Operation::Rename(rename) if rename.kind != ObjectKind::Index => {
            let Some(key) = key(&rename.object) else {
                schema.invalidate();
                return;
            };
            match (rename.kind, rename.member.as_deref()) {
                (ObjectKind::Relation, _) => schema.rename_table(&key, &rename.new_name),
                (ObjectKind::Column, Some(old)) => {
                    schema.rename_column(&key, old, &rename.new_name)
                }
                (ObjectKind::Constraint, Some(old)) => {
                    schema.rename_constraint(&key, old, &rename.new_name);
                }
                _ => (),
            }
        }
        Operation::Drop(drop) if drop.kind == ObjectKind::Relation && !drop.cascade => {
            for relation in &drop.objects {
                let Some(key) = key(relation) else {
                    schema.invalidate();
                    return;
                };
                schema.drop_table(key);
            }
        }
        Operation::Drop(drop)
            if drop.cascade || matches!(drop.kind, ObjectKind::Relation | ObjectKind::Schema) =>
        {
            schema.invalidate();
        }
        Operation::Insert(_) | Operation::Update(_) | Operation::Delete(_) => {
            // Triggers and partition routing can change a different relation.
            schema.mark_data_write();
        }
        _ => (),
    }
}

struct Ddl<'a> {
    migration: &'a ParsedMigration,
    target: PostgresTarget,
    options: &'a RuleOptions,
    schema: &'a Schema,
    finds: &'a mut Vec<Finding>,
    gaps: &'a mut Vec<CoverageGap>,
    locks: &'a mut Vec<RelationLockObservation>,
    complete_statement: bool,
    predictable_lock: bool,
}

impl Ddl<'_> {
    fn inspect(&mut self, statement: &Statement) {
        let span = &statement.span;
        match &statement.operation {
            Operation::AlterTable(alter) => self.alter(span, alter),
            Operation::Rename(rename) => self.rename(span, rename),
            Operation::Drop(drop) => self.drop_objects(span, drop),
            Operation::Truncate(truncate) => {
                for relation in &truncate.relations {
                    let key = self.key(relation);
                    let table = self.schema.table(key.as_ref());
                    self.destructive(
                        span,
                        relation_object(relation, key.as_ref()),
                        table,
                        truncate.cascade,
                    );
                    self.lock(span, relation, RelationLock::AccessExclusive);
                }
            }
            Operation::CreateTable(table) => {
                if let Some(parent) = &table.partition_of {
                    self.lock(span, parent, RelationLock::AccessExclusive);
                }
            }
            Operation::CreateIndex(index) => {
                let key = self.key(&index.relation);
                let table = self.schema.table(key.as_ref());
                let known_partitioned =
                    table.is_some_and(|table| table.kind == Some(RelationKind::PartitionedTable));
                if index.concurrently {
                    if !known_partitioned {
                        self.lock(span, &index.relation, RelationLock::ShareUpdateExclusive);
                    }
                } else {
                    self.lock(span, &index.relation, RelationLock::Share);
                    self.index_build(span, relation_object(&index.relation, key.as_ref()), table);
                }
            }
            _ => (),
        }
    }

    fn key(&self, name: &RelationName) -> Option<RelationKey> {
        resolve(name, &self.options.search_path)
    }

    fn alter(&mut self, span: &SourceSpan, alter: &AlterTable) {
        let key = self.key(&alter.relation);
        let table = self.schema.table(key.as_ref());
        if self.complete_statement && alter.if_exists && self.schema.known_absent(key.as_ref()) {
            return;
        }
        if let Some(mode) = alter_lock(&alter.actions)
            && (mode == RelationLock::AccessExclusive
                || alter
                    .actions
                    .iter()
                    .all(|action| !matches!(action.change, AlterChange::Unsupported)))
        {
            self.lock(span, &alter.relation, mode);
        }

        let mut changed_columns = BTreeSet::new();
        let mut removed_constraints = BTreeSet::new();
        let mut removes_columns = false;
        for action in &alter.actions {
            match &action.change {
                AlterChange::DropColumn { name, .. } => {
                    changed_columns.insert(name.as_str());
                    removes_columns = true;
                }
                AlterChange::DropNotNull { column } | AlterChange::ChangeType { column, .. } => {
                    changed_columns.insert(column.as_str());
                }
                AlterChange::DropConstraint { name, .. } => {
                    removed_constraints.insert(name.as_str());
                }
                _ => (),
            }
        }

        let table_object = || relation_object(&alter.relation, key.as_ref());
        for action in &alter.actions {
            let span = &action.span;
            match &action.change {
                AlterChange::AddColumn {
                    column,
                    if_not_exists,
                } => {
                    if self.complete_statement
                        && *if_not_exists
                        && table.is_some_and(|table| table.has_column(&column.name))
                    {
                        continue;
                    }
                    self.add_column(span, table_object(), table, column);
                }
                AlterChange::DropColumn {
                    name,
                    if_exists,
                    cascade,
                } => {
                    if self.complete_statement
                        && *if_exists
                        && table.is_some_and(|table| table.column_absent(name))
                    {
                        continue;
                    }
                    let confidence = if table.is_some_and(|table| table.has_column(name)) {
                        Confidence::High
                    } else {
                        Confidence::Medium
                    };
                    self.destructive_with_confidence(
                        span,
                        member_object(table_object(), ObjectKind::Column, name),
                        table,
                        *cascade,
                        confidence,
                    );
                }
                AlterChange::SetNotNull { column } => {
                    let proven = table.is_some_and(|table| {
                        table.proves_not_null(
                            column,
                            &changed_columns,
                            &removed_constraints,
                            removes_columns,
                        )
                    });
                    if !proven
                        || !self.complete_statement
                        || !alter.only
                            && table.is_some_and(|table| table.kind != Some(RelationKind::Table))
                    {
                        self.scan(
                            span,
                            table_object(),
                            table,
                            "SET NOT NULL can scan the table for existing null values.",
                            "No retained validated constraint establishes that the column is non-null.",
                        );
                    }
                }
                AlterChange::ChangeType {
                    column,
                    data_type,
                    using_expression,
                } => {
                    let old = table.and_then(|table| table.column_type(column));
                    if !self.complete_statement
                        || !metadata_only_type_change(
                            old,
                            data_type,
                            *using_expression,
                            &self.options.search_path,
                        )
                    {
                        self.rewrite(
                            span,
                            table_object(),
                            table,
                            "Changing this column type can rewrite the table and rebuild indexes.",
                            if old.is_some() { Confidence::Medium } else { Confidence::Low },
                            if *using_expression {
                                "A USING expression may transform stored values. Equivalent casts require additional evidence."
                            } else {
                                "No supported binary-compatible cast or unchanged type rules out a rewrite."
                            },
                        );
                    }
                    if old != Some(data_type) || *using_expression {
                        self.compatibility(
                            span,
                            member_object(table_object(), ObjectKind::Column, column),
                            Severity::Medium,
                            "Changing a column type can change values or types expected by existing clients.",
                        );
                    }
                }
                AlterChange::AddConstraint(constraint) => {
                    self.add_constraint(span, table_object(), table, constraint);
                    if let ConstraintKind::ForeignKey {
                        referenced_table, ..
                    } = &constraint.kind
                    {
                        self.lock(span, referenced_table, RelationLock::ShareRowExclusive);
                    }
                }
                AlterChange::ValidateConstraint { name } => {
                    let constraint = table.and_then(|table| table.constraint(name));
                    if !self.complete_statement
                        || constraint
                            .is_none_or(|constraint| constraint.not_valid && constraint.enforced)
                    {
                        self.scan(
                            span,
                            table_object(),
                            table,
                            "Validating a constraint can scan existing rows.",
                            "A standalone VALIDATE CONSTRAINT requests SHARE UPDATE EXCLUSIVE on the constrained table.",
                        );
                    }
                }
                _ => (),
            }
        }
    }

    fn add_column(
        &mut self,
        span: &SourceSpan,
        object: DatabaseObject,
        table: Option<&TableFacts>,
        column: &ColumnDefinition,
    ) {
        if column.default == DefaultExpr::Volatile {
            self.rewrite(
                span,
                object.clone(),
                table,
                "A volatile default on the added column can rewrite the table.",
                Confidence::High,
                "PostgreSQL computes the volatile default for existing rows. ALTER TABLE ADD COLUMN requests ACCESS EXCLUSIVE.",
            );
        } else if self.complete_statement
            && !self.schema.is_new_empty_table(table)
            && builtin_type(&column.data_type, &self.options.search_path).is_none()
        {
            self.gap(
                span,
                CoverageGapKind::UnresolvedDependency,
                "column type is needed to determine whether a constrained domain forces a rewrite",
            );
        }
        if column.not_null && column.default == DefaultExpr::Absent {
            self.scan(
                span,
                object.clone(),
                table,
                "Adding a NOT NULL column without a default can scan existing rows and reject the migration.",
                "Existing rows would receive NULL for the new column unless another operation establishes a value.",
            );
        }
        for constraint in &column.constraints {
            if !matches!(constraint.kind, ConstraintKind::NotNull) {
                self.add_constraint(span, object.clone(), table, constraint);
            }
        }
    }

    fn add_constraint(
        &mut self,
        span: &SourceSpan,
        object: DatabaseObject,
        table: Option<&TableFacts>,
        constraint: &Constraint,
    ) {
        if !constraint.enforced {
            return;
        }
        match constraint.kind {
            ConstraintKind::Check { .. }
            | ConstraintKind::ForeignKey { .. }
            | ConstraintKind::NotNull
                if !constraint.not_valid =>
            {
                self.scan(
                    span,
                    object,
                    table,
                    "Adding this validated constraint can scan existing rows.",
                    "The statement checks existing rows before commit. A supported NOT VALID form can defer that check.",
                );
            }
            ConstraintKind::PrimaryKey | ConstraintKind::Unique if self.complete_statement => {
                self.index_build(span, object, table);
            }
            _ => (),
        }
    }

    fn rename(&mut self, span: &SourceSpan, rename: &Rename) {
        let key = self.key(&rename.object);
        let table = self.schema.table(key.as_ref());
        if self.complete_statement && rename.if_exists && self.schema.known_absent(key.as_ref()) {
            return;
        }
        if matches!(
            rename.kind,
            ObjectKind::Relation | ObjectKind::Column | ObjectKind::Constraint
        ) {
            self.lock(span, &rename.object, RelationLock::AccessExclusive);
        }
        if self.complete_statement && self.schema.is_private(table) {
            return;
        }
        let known = match (rename.kind, rename.member.as_deref()) {
            (ObjectKind::Relation, _) => table.is_some(),
            (ObjectKind::Column, Some(member)) => {
                table.is_some_and(|table| table.has_column(member))
            }
            (ObjectKind::Constraint, Some(member)) => {
                table.is_some_and(|table| table.constraint(member).is_some())
            }
            _ => false,
        };
        let mut object = relation_object(&rename.object, key.as_ref());
        object.kind = rename.kind;
        object.member.clone_from(&rename.member);
        self.compatibility(
            &rename.action_span,
            object,
            if known {
                Severity::High
            } else {
                Severity::Medium
            },
            "Renaming this object can break clients and operational queries that use its old name.",
        );
    }

    fn drop_objects(&mut self, span: &SourceSpan, drop: &DropObjects) {
        for relation in &drop.objects {
            let key = self.key(relation);
            let table = self.schema.table(key.as_ref());
            let mut object = relation_object(relation, key.as_ref());
            object.kind = drop.kind;
            match drop.kind {
                ObjectKind::Relation => {
                    if self.complete_statement
                        && drop.if_exists
                        && self.schema.known_absent(key.as_ref())
                    {
                        continue;
                    }
                    self.destructive(span, object, table, drop.cascade);
                    self.lock(span, relation, RelationLock::AccessExclusive);
                }
                ObjectKind::Index => {
                    self.finding(
                        span,
                        object,
                        Risk {
                            rule: Rule::Pg005,
                            severity: Severity::Medium,
                            confidence: Confidence::Medium,
                            explanation: "Dropping an index can regress queries that rely on it.",
                            impact: "Queries may use more resources or wait longer after the index is gone.",
                            mitigation: "Review current query plans and dependent constraints before dropping the index.",
                            evidence: if drop.concurrently {
                                "CONCURRENTLY avoids the standard table lock. Workload impact still depends on index usage."
                            } else {
                                "A regular DROP INDEX requests ACCESS EXCLUSIVE on its owning table. Index usage is unknown."
                            },
                        },
                    );
                }
                ObjectKind::Schema | ObjectKind::Type => {
                    self.compatibility(
                        span,
                        object,
                        Severity::Medium,
                        "Removing this object can break clients that still reference it.",
                    );
                    if drop.cascade {
                        self.gap(
                            span,
                            CoverageGapKind::UnresolvedDependency,
                            "cascading schema or type removal may affect relations outside the known inventory",
                        );
                    }
                }
                _ => (),
            }
        }
    }

    fn destructive(
        &mut self,
        span: &SourceSpan,
        object: DatabaseObject,
        table: Option<&TableFacts>,
        cascade: bool,
    ) {
        self.destructive_with_confidence(
            span,
            object,
            table,
            cascade,
            if table.is_some() {
                Confidence::High
            } else {
                Confidence::Medium
            },
        );
    }

    fn destructive_with_confidence(
        &mut self,
        span: &SourceSpan,
        object: DatabaseObject,
        table: Option<&TableFacts>,
        cascade: bool,
        confidence: Confidence,
    ) {
        let proven_empty =
            self.complete_statement && self.schema.is_new_empty_table(table) && !cascade;
        self.finding(
            span,
            object,
            Risk {
                rule: Rule::Pg001,
                severity: if proven_empty { Severity::Low } else { Severity::Critical },
                confidence,
                explanation: "This operation removes stored data or makes it inaccessible.",
                impact: "Recovery may require a retained copy or restore.",
                mitigation: "Stage the removal and verify dependencies and a recovery path before deployment.",
                evidence: if cascade {
                    "CASCADE can affect dependent objects whose state is unavailable to offline analysis."
                } else if proven_empty {
                    "The ordinary table was created in the same transaction with no intervening data writes."
                } else {
                    "The operation can affect existing data, and the migration does not establish an empty private table."
                },
            },
        );
    }

    fn compatibility(
        &mut self,
        span: &SourceSpan,
        object: DatabaseObject,
        severity: Severity,
        explanation: &'static str,
    ) {
        self.finding(
            span,
            object,
            Risk {
                rule: Rule::Pg002,
                severity,
                confidence: if severity == Severity::High { Confidence::High } else { Confidence::Medium },
                explanation,
                impact: "Existing application versions may fail during a rolling deployment.",
                mitigation: "Use an expand-and-contract rollout and verify clients before removing the old contract.",
                evidence: "The compatibility prediction uses migration facts. Application dependencies are outside offline evidence.",
            },
        );
    }

    fn rewrite(
        &mut self,
        span: &SourceSpan,
        object: DatabaseObject,
        table: Option<&TableFacts>,
        explanation: &'static str,
        confidence: Confidence,
        evidence: &'static str,
    ) {
        if self.complete_statement && self.schema.is_new_empty_table(table) {
            return;
        }
        self.finding(
            span,
            object,
            Risk {
                rule: Rule::Pg003,
                severity: Severity::High,
                confidence,
                explanation,
                impact: "A rewrite can block access and require substantial time and disk space.",
                mitigation: "Stage an additive column and bounded backfill where feasible, and rehearse the change on representative data.",
                evidence,
            },
        );
    }

    fn scan(
        &mut self,
        span: &SourceSpan,
        object: DatabaseObject,
        table: Option<&TableFacts>,
        explanation: &'static str,
        evidence: &'static str,
    ) {
        if self.complete_statement && self.schema.is_new_empty_table(table) {
            return;
        }
        self.finding(
            span,
            object,
            Risk {
                rule: Rule::Pg004,
                severity: Severity::Medium,
                confidence: if table.is_some() { Confidence::High } else { Confidence::Medium },
                explanation,
                impact: "Validation can take time proportional to existing data and holds the statement's lock until transaction end.",
                mitigation: "When supported, add the constraint NOT VALID and validate in a separate transaction.",
                evidence,
            },
        );
    }

    fn index_build(
        &mut self,
        span: &SourceSpan,
        object: DatabaseObject,
        table: Option<&TableFacts>,
    ) {
        if self.complete_statement && self.schema.is_private(table) {
            return;
        }
        self.finding(
            span,
            object,
            Risk {
                rule: Rule::Pg005,
                severity: Severity::High,
                confidence: if table.is_some() { Confidence::High } else { Confidence::Medium },
                explanation: "A regular index build blocks concurrent writes to the table.",
                impact: "Application inserts, updates, and deletes can wait until the index build commits.",
                mitigation: "Where the target and runner permit it, create the index concurrently outside a transaction block. Check for an invalid index before retrying a failed build.",
                evidence: "The index build lacks CONCURRENTLY and the migration does not establish that the table is private to this transaction.",
            },
        );
    }

    fn finding(&mut self, span: &SourceSpan, object: DatabaseObject, risk: Risk) {
        let finding = record(
            self.finds,
            self.migration,
            span,
            FindingSpec {
                rule: risk.rule,
                object: Some(object),
                severity: risk.severity,
                confidence: risk.confidence,
                explanation: risk.explanation,
                impact: risk.impact,
                mitigation: risk.mitigation,
                evidence: risk.evidence,
            },
        );
        finding.documentation = match risk.rule {
            Rule::Pg005 if risk.severity == Severity::Medium => {
                format!(
                    "https://www.postgresql.org/docs/{}/sql-dropindex.html",
                    self.target
                )
            }
            _ if self.target < PostgresTarget::V18 => finding
                .documentation
                .replace("/18/", &format!("/{}/", self.target)),
            _ => return,
        };
    }

    fn lock(&mut self, span: &SourceSpan, relation: &RelationName, mode: RelationLock) {
        if self.predictable_lock {
            self.locks.push(RelationLockObservation {
                migration: self.migration.identity.clone(),
                span: span.clone(),
                object: relation_object(relation, self.key(relation).as_ref()),
                mode,
            });
        }
    }

    fn gap(&mut self, span: &SourceSpan, kind: CoverageGapKind, message: &'static str) {
        self.gaps.push(CoverageGap {
            scope: DiagnosticScope::Span { span: span.clone() },
            kind,
            message: message.into(),
            related_paths: Vec::new(),
        });
    }
}

struct Risk {
    rule: Rule,
    severity: Severity,
    confidence: Confidence,
    explanation: &'static str,
    impact: &'static str,
    mitigation: &'static str,
    evidence: &'static str,
}

fn relation_object(relation: &RelationName, key: Option<&RelationKey>) -> DatabaseObject {
    DatabaseObject {
        kind: ObjectKind::Relation,
        schema: key
            .map(|key| key.schema.clone())
            .or_else(|| relation.schema.clone()),
        name: relation.name.clone(),
        member: None,
    }
}

fn member_object(mut object: DatabaseObject, kind: ObjectKind, name: &str) -> DatabaseObject {
    object.kind = kind;
    object.member = Some(name.into());
    object
}

/// Strongest known relation lock from the supported subcommands.
pub(crate) fn alter_lock(actions: &[AlterAction]) -> Option<RelationLock> {
    actions
        .iter()
        .filter_map(|action| {
            Some(match &action.change {
                AlterChange::AddConstraint(constraint)
                    if matches!(constraint.kind, ConstraintKind::ForeignKey { .. }) =>
                {
                    RelationLock::ShareRowExclusive
                }
                AlterChange::ValidateConstraint { .. } => RelationLock::ShareUpdateExclusive,
                AlterChange::Unsupported => return None,
                _ => RelationLock::AccessExclusive,
            })
        })
        .max()
}

fn metadata_only_type_change(
    old: Option<&DataType>,
    new: &DataType,
    using_expression: bool,
    search_path: &[String],
) -> bool {
    let Some(old) = old else {
        return false;
    };
    if using_expression {
        return false;
    }
    let (Some(old_name), Some(new_name)) = (
        builtin_type(old, search_path),
        builtin_type(new, search_path),
    ) else {
        return false;
    };
    if old == new {
        return old.integer_modifiers.is_empty();
    }
    old.array_dimensions == 0
        && new.array_dimensions == 0
        && new.integer_modifiers.is_empty()
        && matches!(old_name, "text" | "varchar")
        && matches!(new_name, "text" | "varchar")
}

fn builtin_type<'a>(data_type: &'a DataType, search_path: &[String]) -> Option<&'a str> {
    let name = match data_type.name.as_slice() {
        [name] if !search_path.iter().any(|schema| schema == "pg_catalog") => name.as_str(),
        [name]
            if search_path
                .first()
                .is_some_and(|schema| schema == "pg_catalog") =>
        {
            name.as_str()
        }
        [schema, name] if schema == "pg_catalog" => name.as_str(),
        _ => return None,
    };
    matches!(
        name,
        "bool"
            | "bpchar"
            | "bytea"
            | "char"
            | "date"
            | "float4"
            | "float8"
            | "int2"
            | "int4"
            | "int8"
            | "interval"
            | "json"
            | "jsonb"
            | "numeric"
            | "oid"
            | "text"
            | "time"
            | "timestamp"
            | "timestamptz"
            | "timetz"
            | "uuid"
            | "varchar"
            | "serial"
            | "bigserial"
            | "smallserial"
            | "serial2"
            | "serial4"
            | "serial8"
    )
    .then_some(name)
}
