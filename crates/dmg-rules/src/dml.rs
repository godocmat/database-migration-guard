//! Broad data changes, execution-block conflicts, and migration ordering checks.

use std::collections::{BTreeMap, BTreeSet};

use dmg_core::{
    coverage::{CoverageGap, CoverageStatus},
    migration::MigrationAdapter,
    risk::{
        Confidence, DatabaseObject, EvidenceKind, Finding, FindingEvidence, ObjectKind, Severity,
    },
    source::SourceSpan,
};
use dmg_postgres::{
    Constraint, ConstraintKind, DataChange, Operation, ParsedMigration, ParsedPlan, Predicate,
    RelationName, Statement,
};

use crate::{
    RelationLock, Rule, RuleOptions,
    ddl::alter_lock,
    finding::{FindingSpec, Selection, record},
    schema::{RelationKey, resolve},
    transaction::{
        TransactionConflict, TransactionContext, TransactionEffect, TransactionTimeline,
    },
};

pub(crate) fn evaluate(
    plan: &ParsedPlan,
    options: &RuleOptions,
    timelines: &[TransactionTimeline],
    findings: &mut Vec<Finding>,
    gaps: &mut Vec<CoverageGap>,
) {
    let selection = Selection::new(plan);
    let ordering = ordering_evidence(plan, options, timelines);
    for (index, (migration, timeline)) in plan.migrations.iter().zip(timelines).enumerate() {
        gaps.extend_from_slice(&timeline.gaps);
        if !selection.contains(&migration.path) {
            continue;
        }

        let mut locks = HeldLocks::default();
        for (position, (statement, step)) in
            migration.statements.iter().zip(&timeline.steps).enumerate()
        {
            if step.starts_block {
                locks.clear();
            }
            if let Some(conflict) = step.conflict {
                transaction_conflict(
                    findings,
                    migration,
                    statement,
                    step.context,
                    conflict,
                    options,
                );
            }
            if let Some(change) = broad_change(&statement.operation) {
                broad_finding(findings, migration, statement, change, options);
                if step.context.shares_transaction() && !change.additional_relations {
                    locks.report(findings, migration, statement, &change.relation, options);
                }
            }
            if !ordering.is_empty() {
                let required: BTreeSet<_> = required_relations(&statement.operation)
                    .into_iter()
                    .filter_map(|name| resolve(name, &options.search_path))
                    .collect();
                for relation in required {
                    if let Some(creation) = ordering.get(&relation)
                        && creation.migration > index
                    {
                        ordering_finding(findings, migration, statement, creation, relation);
                    }
                }
            }
            if step.context.shares_transaction() && step.conflict.is_none() {
                locks.observe(statement, position, options);
            }
            locks.apply(step.effect);
        }
    }
}

fn relation_object(name: &RelationName, options: &RuleOptions) -> DatabaseObject {
    let key = resolve(name, &options.search_path);
    DatabaseObject {
        kind: ObjectKind::Relation,
        schema: key.map(|key| key.schema).or_else(|| name.schema.clone()),
        name: name.name.clone(),
        member: None,
    }
}

fn broad_change(operation: &Operation) -> Option<&DataChange> {
    match operation {
        Operation::Update(change) | Operation::Delete(change)
            if matches!(change.predicate, Predicate::Absent | Predicate::AlwaysTrue) =>
        {
            Some(change)
        }
        _ => None,
    }
}

fn broad_finding(
    findings: &mut Vec<Finding>,
    migration: &ParsedMigration,
    statement: &Statement,
    change: &DataChange,
    options: &RuleOptions,
) {
    let (explanation, impact) = if matches!(&statement.operation, Operation::Update(_)) {
        (
            "UPDATE has no limiting predicate.",
            "The migration can modify every target row and hold row locks while the transaction is open.",
        )
    } else {
        (
            "DELETE has no limiting predicate.",
            "The migration can remove every target row and hold row locks while the transaction is open.",
        )
    };
    let predicate = if change.predicate == Predicate::Absent {
        "There is no WHERE clause."
    } else {
        "The WHERE clause is true independently of the row values."
    };
    let evidence = if change.additional_relations {
        format!("{predicate} Additional source relations still affect how many target rows change.")
    } else {
        predicate.into()
    };
    record(
        findings,
        migration,
        &statement.span,
        FindingSpec {
            rule: Rule::Pg006,
            object: Some(relation_object(&change.relation, options)),
            severity: Severity::High,
            confidence: if change.additional_relations {
                Confidence::Medium
            } else {
                Confidence::High
            },
            explanation,
            impact,
            mitigation: "Restrict the target rows explicitly. Review the affected set and batch large changes using a stable key.",
            evidence: &evidence,
        },
    );
}

fn transaction_conflict(
    findings: &mut Vec<Finding>,
    migration: &ParsedMigration,
    statement: &Statement,
    context: TransactionContext,
    conflict: TransactionConflict,
    options: &RuleOptions,
) {
    let object = match &statement.operation {
        Operation::CreateIndex(index) => Some(relation_object(&index.relation, options)),
        Operation::Drop(drop) => drop.objects.first().map(|index| DatabaseObject {
            kind: ObjectKind::Index,
            schema: index.schema.clone(),
            name: index.name.clone(),
            member: None,
        }),
        _ => None,
    };
    let (explanation, mitigation) = match conflict {
        TransactionConflict::ConcurrentIndex => {
            let explanation = if matches!(&statement.operation, Operation::CreateIndex(_)) {
                "CREATE INDEX CONCURRENTLY cannot run inside this transaction block."
            } else {
                "DROP INDEX CONCURRENTLY cannot run inside this transaction block."
            };
            let mitigation = match migration.identity.adapter {
                MigrationAdapter::Sqlx => {
                    "Put this operation alone in an SQLx migration with -- no-transaction at byte zero."
                }
                MigrationAdapter::Goose => {
                    "Use a Goose NO TRANSACTION migration and put the index operation in its own query."
                }
                MigrationAdapter::Plain | MigrationAdapter::Flyway | MigrationAdapter::Supabase => {
                    "Run this operation as an autocommit statement outside the migration's transaction block."
                }
            };
            (explanation, mitigation)
        }
        TransactionConflict::ChainOutsideBlock => (
            "The chained transaction command requires a regular transaction block.",
            "Place the chained command inside an explicit block or remove the chain.",
        ),
        TransactionConflict::SavepointOutsideBlock => (
            "The savepoint command requires a regular transaction block.",
            "Use explicit transaction boundaries around the savepoint and the work it controls.",
        ),
        TransactionConflict::MissingSavepoint => (
            "No active savepoint in this transaction matches the command.",
            "Define the savepoint before referring to it and preserve it until this command runs.",
        ),
    };
    let (evidence, impact) = match context {
        TransactionContext::Runner => (
            "The configured runner opens a transaction before the migration.",
            "PostgreSQL rejects the command and the active runner transaction can roll back.",
        ),
        TransactionContext::Explicit => (
            "A transaction opened by the migration is active here.",
            "PostgreSQL rejects the command and the active transaction can roll back. Earlier committed work can remain.",
        ),
        TransactionContext::ImplicitBatch => (
            "The runner sends multiple statements in one simple Query, so PostgreSQL opens an implicit transaction block.",
            "PostgreSQL rejects the command and can roll back earlier changes in this implicit block.",
        ),
        TransactionContext::ExtendedBatch => (
            "The runner submits an extended-query batch in an implicit transaction block.",
            "PostgreSQL rejects the command and can roll back earlier changes in this batch.",
        ),
        TransactionContext::Standalone | TransactionContext::Unknown => (
            "The statement has no regular transaction block available.",
            "PostgreSQL rejects the command. Earlier committed work can remain.",
        ),
    };
    record(
        findings,
        migration,
        &statement.span,
        FindingSpec {
            rule: Rule::Pg007,
            object,
            severity: Severity::High,
            confidence: Confidence::High,
            explanation,
            impact,
            mitigation,
            evidence,
        },
    );
}

struct Creation<'a> {
    migration: usize,
    span: &'a SourceSpan,
}

/// Find table definitions whose identity and position are known from supported SQL.
fn ordering_evidence<'a>(
    plan: &'a ParsedPlan,
    options: &RuleOptions,
    timelines: &[TransactionTimeline],
) -> BTreeMap<RelationKey, Creation<'a>> {
    let mut candidates = BTreeMap::<_, Option<_>>::new();
    for (index, (migration, timeline)) in plan.migrations.iter().zip(timelines).enumerate() {
        if migration.coverage.status() != CoverageStatus::Complete
            || (index > 0 && plan.migrations[index - 1].identity >= migration.identity)
            || timeline.end_effect == TransactionEffect::Rollback
            || timeline.end_effect == TransactionEffect::Invalidate
            || timeline.steps.iter().any(|step| {
                matches!(
                    step.effect,
                    TransactionEffect::Rollback
                        | TransactionEffect::RollbackTo(_)
                        | TransactionEffect::Invalidate
                )
            })
        {
            // Effects beyond an opaque or potentially reverted migration are unproven.
            break;
        }

        // A lifecycle operation with an unresolved target can hide an earlier definition.
        if migration
            .statements
            .iter()
            .any(|statement| obscures_lifecycle(&statement.operation, options))
        {
            break;
        }

        for statement in &migration.statements {
            match &statement.operation {
                Operation::CreateTable(table) => {
                    let key = resolve(&table.relation, &options.search_path)
                        .expect("creation target checked before replay");
                    let creation = (!table.if_not_exists).then_some(Creation {
                        migration: index,
                        span: &statement.span,
                    });
                    candidates
                        .entry(key)
                        .and_modify(|value| *value = None)
                        .or_insert(creation);
                }
                Operation::Rename(rename) if rename.kind == ObjectKind::Relation => {
                    let key = resolve(&rename.object, &options.search_path)
                        .expect("rename target checked before replay");
                    let new_key = RelationKey {
                        schema: key.schema.clone(),
                        name: rename.new_name.clone(),
                    };
                    candidates.insert(key, None);
                    candidates.insert(new_key, None);
                }
                Operation::Drop(drop) if drop.kind == ObjectKind::Relation && !drop.cascade => {
                    for object in &drop.objects {
                        let key = resolve(object, &options.search_path)
                            .expect("drop target checked before replay");
                        candidates.entry(key).or_insert(None);
                    }
                }
                _ => {}
            }
        }
    }
    candidates
        .into_iter()
        .filter_map(|(key, creation)| creation.map(|creation| (key, creation)))
        .collect()
}

fn obscures_lifecycle(operation: &Operation, options: &RuleOptions) -> bool {
    let unresolved = |name| resolve(name, &options.search_path).is_none();
    match operation {
        Operation::CreateTable(table) => unresolved(&table.relation),
        Operation::Rename(rename) if rename.kind == ObjectKind::Relation => {
            unresolved(&rename.object)
        }
        Operation::Drop(drop) if drop.kind == ObjectKind::Relation => {
            drop.cascade || drop.objects.iter().any(unresolved)
        }
        Operation::Drop(drop) if drop.kind == ObjectKind::Schema => true,
        _ => false,
    }
}

fn foreign_table(constraint: &Constraint) -> Option<&RelationName> {
    if let ConstraintKind::ForeignKey {
        referenced_table, ..
    } = &constraint.kind
    {
        Some(referenced_table)
    } else {
        None
    }
}

fn required_relations(operation: &Operation) -> Vec<&RelationName> {
    match operation {
        Operation::AlterTable(table) if !table.if_exists => {
            let mut relations = vec![&table.relation];
            relations.extend(table.actions.iter().filter_map(|action| {
                if let dmg_postgres::AlterChange::AddConstraint(constraint) = &action.change {
                    foreign_table(constraint)
                } else {
                    None
                }
            }));
            relations
        }
        Operation::CreateTable(table) if !table.if_not_exists => table
            .partition_of
            .iter()
            .chain(table.constraints.iter().filter_map(foreign_table))
            .chain(
                table
                    .columns
                    .iter()
                    .flat_map(|column| column.constraints.iter().filter_map(foreign_table)),
            )
            .filter(|name| *name != &table.relation)
            .collect(),
        Operation::Rename(rename) if !rename.if_exists && rename.kind != ObjectKind::Index => {
            vec![&rename.object]
        }
        Operation::Drop(drop) if drop.kind == ObjectKind::Relation && !drop.if_exists => {
            drop.objects.iter().collect()
        }
        Operation::Truncate(truncate) => truncate.relations.iter().collect(),
        Operation::CreateIndex(index) if !index.if_not_exists => vec![&index.relation],
        Operation::Insert(relation) => vec![relation],
        Operation::Update(change) | Operation::Delete(change) => vec![&change.relation],
        _ => Vec::new(),
    }
}

fn ordering_finding(
    findings: &mut Vec<Finding>,
    migration: &ParsedMigration,
    statement: &Statement,
    creation: &Creation<'_>,
    relation: RelationKey,
) {
    let finding = record(
        findings,
        migration,
        &statement.span,
        FindingSpec {
            rule: Rule::Pg007,
            object: Some(DatabaseObject {
                kind: ObjectKind::Relation,
                schema: Some(relation.schema),
                name: relation.name,
                member: None,
            }),
            severity: Severity::High,
            confidence: Confidence::High,
            explanation: "The migration uses a table before its recorded creation.",
            impact: "A replay in migration order can fail because the required table has not been created.",
            mitigation: "Create the relation in an earlier migration or move this operation after the creating migration.",
            evidence: "The analyzed forward sequence through the defining migration has an unconditional creation of this table after the reference, with no earlier supported definition.",
        },
    );
    finding.evidence.push(FindingEvidence {
        kind: EvidenceKind::InputFact,
        detail: format!(
            "The creating statement starts at {}:{}.",
            creation.span.path, creation.span.start.line
        ),
        artifact_digest: None,
    });
}

#[derive(Default)]
struct HeldLocks<'a> {
    // Keep each acquisition so a savepoint rollback can restore an earlier weaker lock.
    relations: BTreeMap<RelationKey, Vec<(usize, RelationLock, &'a SourceSpan)>>,
    // A relation created by this migration has no established external workload yet.
    created: BTreeMap<RelationKey, usize>,
}

impl<'a> HeldLocks<'a> {
    fn clear(&mut self) {
        self.relations.clear();
        self.created.clear();
    }

    fn apply(&mut self, effect: TransactionEffect) {
        match effect {
            TransactionEffect::Commit
            | TransactionEffect::Rollback
            | TransactionEffect::Invalidate => {
                self.clear();
            }
            TransactionEffect::RollbackTo(marker) => {
                self.relations.retain(|_, acquired| {
                    acquired.retain(|(position, _, _)| *position < marker);
                    !acquired.is_empty()
                });
                self.created.retain(|_, position| *position < marker);
            }
            TransactionEffect::None
            | TransactionEffect::Savepoint(_)
            | TransactionEffect::Release(_) => {}
        }
    }

    fn observe(&mut self, statement: &'a Statement, position: usize, options: &RuleOptions) {
        let (relation, mode) = match &statement.operation {
            Operation::CreateTable(table) => {
                if let Some(key) = resolve(&table.relation, &options.search_path) {
                    self.relations.remove(&key);
                    if !table.if_not_exists {
                        self.created.insert(key, position);
                    }
                }
                return;
            }
            Operation::AlterTable(table) if !table.if_exists => {
                let Some(mode) = alter_lock(&table.actions) else {
                    return;
                };
                (&table.relation, mode)
            }
            Operation::CreateIndex(index) if !index.concurrently && !index.if_not_exists => {
                (&index.relation, RelationLock::Share)
            }
            Operation::Rename(_) | Operation::Drop(_) | Operation::Unsupported => {
                self.clear();
                return;
            }
            _ => return,
        };
        if !matches!(
            mode,
            RelationLock::Share | RelationLock::ShareRowExclusive | RelationLock::AccessExclusive
        ) {
            return;
        }
        let Some(key) = resolve(relation, &options.search_path) else {
            return;
        };
        if !self.created.contains_key(&key) {
            self.relations
                .entry(key)
                .or_default()
                .push((position, mode, &statement.span));
        }
    }

    fn report(
        &self,
        findings: &mut Vec<Finding>,
        migration: &ParsedMigration,
        statement: &Statement,
        relation: &RelationName,
        options: &RuleOptions,
    ) {
        let Some(key) = resolve(relation, &options.search_path) else {
            return;
        };
        let Some((_, mode, acquired)) = self
            .relations
            .get(&key)
            .and_then(|locks| locks.iter().max_by_key(|(_, mode, _)| *mode))
        else {
            return;
        };
        let lock = match mode {
            RelationLock::Share => "SHARE",
            RelationLock::ShareRowExclusive => "SHARE ROW EXCLUSIVE",
            RelationLock::AccessExclusive => "ACCESS EXCLUSIVE",
            RelationLock::ShareUpdateExclusive => {
                unreachable!("writer-compatible locks are not retained")
            }
        };
        let evidence = format!(
            "The statement at {}:{} can acquire {}. The broad data change runs before this transaction releases that lock.",
            acquired.path, acquired.start.line, lock
        );
        record(
            findings,
            migration,
            &statement.span,
            FindingSpec {
                rule: Rule::Pg007,
                object: Some(relation_object(relation, options)),
                severity: Severity::High,
                confidence: Confidence::High,
                explanation: "A writer-blocking relation lock can remain held during a broad data change.",
                impact: "Concurrent writes can wait while the migration processes the affected rows and finishes the transaction.",
                mitigation: "Commit the lock-taking DDL separately, then apply the data change in bounded transactions.",
                evidence: &evidence,
            },
        );
    }
}

#[cfg(test)]
mod tests {
    use dmg_core::{
        coverage::Coverage,
        digest::ContentDigest,
        migration::{ExecutionMode, MigrationDirection, MigrationFile, MigrationIdentity},
        source::{ByteRange, RepoPath},
        target::PostgresTarget,
    };
    use dmg_postgres::parse_migration;

    use super::*;
    use crate::transaction::timelines;

    fn parse(version: i64, sql: &str, execution: ExecutionMode) -> ParsedMigration {
        let file = MigrationFile {
            identity: match execution {
                ExecutionMode::SUPABASE_BATCH => {
                    MigrationIdentity::supabase_version(&version.to_string()).unwrap()
                }
                ExecutionMode::SQLX_TRANSACTION | ExecutionMode::SQLX_NO_TRANSACTION => {
                    MigrationIdentity::numeric(MigrationAdapter::Sqlx, version)
                }
                _ => MigrationIdentity::numeric(MigrationAdapter::Plain, version),
            },
            path: RepoPath::new(format!("migrations/{version}.sql")).unwrap(),
            direction: MigrationDirection::Forward,
            digest: ContentDigest::of(sql.as_bytes()),
            source_segments: vec![ByteRange::new(0, sql.len()).unwrap()],
            execution,
        };
        let parsed = parse_migration(&file, sql.as_bytes(), PostgresTarget::V18);
        assert_eq!(parsed.coverage, Coverage::Complete);
        parsed
    }

    fn analyze(migrations: Vec<ParsedMigration>) -> (Vec<Finding>, Vec<CoverageGap>) {
        let plan = ParsedPlan {
            migrations,
            selected_forwards: None,
            coverage: Coverage::Complete,
        };
        let mut findings = Vec::new();
        let mut gaps = Vec::new();
        evaluate(
            &plan,
            &RuleOptions::default(),
            &timelines(&plan),
            &mut findings,
            &mut gaps,
        );
        (findings, gaps)
    }

    fn assert_rules(findings: &[Finding], expected: &[Rule]) {
        let ids: Vec<_> = findings.iter().map(|finding| finding.rule_id).collect();
        let expected: Vec<_> = expected.iter().map(|rule| rule.id()).collect();
        assert_eq!(ids, expected, "{findings:#?}");
    }

    #[test]
    fn finds_unbounded_updates_and_deletes_without_treating_unknown_predicates_as_true() {
        let sql = "UPDATE app.accounts SET active = false;
            DELETE FROM app.accounts WHERE false OR (true AND 4 = 4);
            DELETE FROM app.accounts WHERE false AND id = 1;
            UPDATE app.accounts SET active = false WHERE id = 1;";
        let (findings, gaps) = analyze(vec![parse(1, sql, ExecutionMode::PSQL_AUTOCOMMIT)]);
        assert!(gaps.is_empty());
        assert_rules(&findings, &[Rule::Pg006, Rule::Pg006]);
        assert_eq!(findings[0].span.bytes.start(), 0);
        assert!(sql[findings[1].span.bytes.as_range()].starts_with("DELETE FROM app.accounts"));
    }

    #[test]
    fn concurrent_index_execution_obeys_wrapper_and_simple_query_boundaries() {
        let index = "CREATE INDEX CONCURRENTLY accounts_idx ON app.accounts (id);";
        for (sql, mode, expected) in [
            (index.to_owned(), ExecutionMode::PSQL_FILE_TRANSACTION, true),
            (index.to_owned(), ExecutionMode::SQLX_TRANSACTION, true),
            (index.to_owned(), ExecutionMode::SQLX_NO_TRANSACTION, false),
            (
                format!("BEGIN; {index} COMMIT;"),
                ExecutionMode::PSQL_AUTOCOMMIT,
                true,
            ),
            (
                format!("BEGIN; COMMIT; {index}"),
                ExecutionMode::PSQL_FILE_TRANSACTION,
                false,
            ),
            (
                format!("BEGIN; COMMIT; {index}"),
                ExecutionMode::SQLX_TRANSACTION,
                true,
            ),
            (
                format!("BEGIN; ROLLBACK; {index}"),
                ExecutionMode::SQLX_NO_TRANSACTION,
                true,
            ),
            (
                "BEGIN; DROP INDEX CONCURRENTLY app.accounts_idx; ROLLBACK;".into(),
                ExecutionMode::PSQL_AUTOCOMMIT,
                true,
            ),
        ] {
            let (findings, gaps) = analyze(vec![parse(1, &sql, mode)]);
            assert!(gaps.is_empty(), "{sql}: {gaps:#?}");
            assert_rules(&findings, if expected { &[Rule::Pg007] } else { &[] });
        }
    }

    #[test]
    fn extended_batch_groups_statements_and_has_no_explicit_savepoint_scope() {
        let ddl = "ALTER TABLE app.accounts ADD COLUMN note text;";
        let update = "UPDATE app.accounts SET note = 'existing';";
        let (findings, gaps) = analyze(vec![parse(
            1,
            &format!("{ddl} {update}"),
            ExecutionMode::SUPABASE_BATCH,
        )]);
        assert!(gaps.is_empty());
        assert_rules(&findings, &[Rule::Pg006, Rule::Pg007]);

        let (findings, gaps) = analyze(vec![parse(
            1,
            &format!("{ddl} SAVEPOINT boundary;"),
            ExecutionMode::SUPABASE_BATCH,
        )]);
        assert!(gaps.is_empty());
        assert_rules(&findings, &[Rule::Pg007]);
        assert!(findings[0].explanation.contains("savepoint"));
    }

    #[test]
    fn retained_writer_blocking_lock_accounts_for_commit_and_savepoint_rollback() {
        let ddl = "ALTER TABLE app.accounts ADD COLUMN note text;";
        let dml = "UPDATE app.accounts SET note = 'existing';";
        for (prefix, between, expect_lock) in [
            ("BEGIN;", "", true),
            ("BEGIN;", "COMMIT;", false),
            ("BEGIN; SAVEPOINT s;", "ROLLBACK TO s;", false),
            ("BEGIN; SAVEPOINT s;", "RELEASE s;", true),
        ] {
            let sql = format!("{prefix} {ddl} {between} {dml} COMMIT;");
            let (findings, gaps) = analyze(vec![parse(1, &sql, ExecutionMode::PSQL_AUTOCOMMIT)]);
            assert!(gaps.is_empty());
            assert_rules(
                &findings,
                if expect_lock {
                    &[Rule::Pg006, Rule::Pg007]
                } else {
                    &[Rule::Pg006]
                },
            );
        }
    }

    #[test]
    fn requires_an_unconditional_later_creation_to_prove_ordering() {
        let alter = "ALTER TABLE app.accounts ADD COLUMN note text;";
        let mut earlier = parse(1, alter, ExecutionMode::PSQL_AUTOCOMMIT);
        let create = "CREATE TABLE app.accounts (id int);";
        let later = parse(2, create, ExecutionMode::PSQL_AUTOCOMMIT);
        let (findings, gaps) = analyze(vec![earlier.clone(), later.clone()]);
        assert!(gaps.is_empty());
        assert_rules(&findings, &[Rule::Pg007]);
        assert!(
            findings[0].evidence[1]
                .detail
                .contains("migrations/2.sql:1")
        );
        assert_eq!(findings[0].span.bytes.as_range(), 0..alter.len() - 1);

        earlier = parse(
            1,
            "ALTER TABLE IF EXISTS app.accounts ADD COLUMN note text;",
            ExecutionMode::PSQL_AUTOCOMMIT,
        );
        assert_rules(&analyze(vec![earlier, later.clone()]).0, &[]);
        assert_rules(
            &analyze(vec![
                parse(1, create, ExecutionMode::PSQL_AUTOCOMMIT),
                parse(2, alter, ExecutionMode::PSQL_AUTOCOMMIT),
                parse(3, create, ExecutionMode::PSQL_AUTOCOMMIT),
            ])
            .0,
            &[],
        );
    }

    #[test]
    fn unresolved_sqlx_boundary_makes_the_next_concurrent_operation_incomplete() {
        let (findings, gaps) = analyze(vec![
            parse(1, "BEGIN;", ExecutionMode::SQLX_NO_TRANSACTION),
            parse(
                2,
                "CREATE INDEX CONCURRENTLY accounts_idx ON app.accounts (id);",
                ExecutionMode::SQLX_NO_TRANSACTION,
            ),
        ]);
        assert_rules(&findings, &[]);
        assert_eq!(gaps.len(), 2);
        assert!(gaps.iter().all(|gap| {
            gap.kind == dmg_core::coverage::CoverageGapKind::UnresolvedTransactionMode
        }));
    }
}
