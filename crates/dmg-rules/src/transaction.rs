//! Map supported migration statements to their PostgreSQL transaction boundaries.

use dmg_core::{
    coverage::{CoverageGap, CoverageGapKind, DiagnosticScope},
    migration::{MigrationAdapter, StatementDelivery, TransactionWrapper},
    risk::ObjectKind,
};
use dmg_postgres::{Operation, ParsedMigration, ParsedPlan, Statement, Transaction};

#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub(crate) enum TransactionContext {
    Standalone,
    Runner,
    Explicit,
    ImplicitBatch,
    ExtendedBatch,
    Unknown,
}

impl TransactionContext {
    fn explicit(self) -> bool {
        matches!(self, Self::Runner | Self::Explicit)
    }

    pub(crate) fn shares_transaction(self) -> bool {
        matches!(
            self,
            Self::Runner | Self::Explicit | Self::ImplicitBatch | Self::ExtendedBatch
        )
    }
}

#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub(crate) enum TransactionEffect {
    None,
    Commit,
    Rollback,
    Savepoint(usize),
    RollbackTo(usize),
    Release(usize),
    Invalidate,
}

#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub(crate) enum TransactionConflict {
    ConcurrentIndex,
    ChainOutsideBlock,
    SavepointOutsideBlock,
    MissingSavepoint,
}

#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub(crate) struct TransactionStep {
    pub(crate) context: TransactionContext,
    pub(crate) starts_block: bool,
    pub(crate) effect: TransactionEffect,
    pub(crate) conflict: Option<TransactionConflict>,
}

pub(crate) struct TransactionTimeline {
    pub(crate) steps: Vec<TransactionStep>,
    pub(crate) end_effect: TransactionEffect,
    pub(crate) gaps: Vec<CoverageGap>,
}

/// A plain SQL runner uses a fresh psql session per migration. SQLx can reuse its connection.
pub(crate) fn timelines(plan: &ParsedPlan) -> Vec<TransactionTimeline> {
    let mut sqlx_connection_uncertain = false;
    plan.migrations
        .iter()
        .map(|migration| {
            let sqlx = migration.identity.adapter == MigrationAdapter::Sqlx;
            let (timeline, leaves_uncertainty) =
                timeline(migration, sqlx && sqlx_connection_uncertain);
            sqlx_connection_uncertain |= sqlx && leaves_uncertainty;
            timeline
        })
        .collect()
}

fn timeline(
    migration: &ParsedMigration,
    uncertain_connection: bool,
) -> (TransactionTimeline, bool) {
    let mut state = BlockState {
        context: TransactionContext::Standalone,
        pending_start: false,
        savepoints: Vec::new(),
        stopped: false,
    };
    if migration.execution.transaction_wrapper == TransactionWrapper::File {
        state.start(TransactionContext::Runner);
    }

    let delivery = migration.execution.statement_delivery;
    let uncertain_at = first_missing_statement(migration);
    if uncertain_connection
        || matches!(
            migration.execution.transaction_wrapper,
            TransactionWrapper::Flyway(_)
        )
        || delivery != StatementDelivery::PerStatement && uncertain_at.is_some()
    {
        // Missing source may change a batch's boundaries and statement count.
        state.invalidate();
    }
    let mut steps = Vec::with_capacity(migration.statements.len());
    let mut gaps = Vec::new();
    for segment in migration
        .statements
        .chunk_by(|left, right| left.segment == right.segment)
    {
        let implicit = match delivery {
            StatementDelivery::SingleQuery if segment.len() > 1 => {
                Some(TransactionContext::ImplicitBatch)
            }
            StatementDelivery::ExtendedBatch => Some(TransactionContext::ExtendedBatch),
            StatementDelivery::PerStatement | StatementDelivery::SingleQuery => None,
        };
        for statement in segment {
            if uncertain_at.is_some_and(|start| start <= statement.span.bytes.start()) {
                state.invalidate();
            }

            // The simple Query loop starts a new implicit block when necessary before
            // every statement, including after an explicit COMMIT or ROLLBACK.
            if let Some(context) = implicit
                && state.context == TransactionContext::Standalone
            {
                state.start(context);
            }
            if state.context == TransactionContext::Unknown
                && !state.stopped
                && requires_standalone(&statement.operation)
            {
                gaps.push(transaction_gap(
                    statement,
                    "could not establish the execution block for this concurrent index operation",
                ));
            }
            steps.push(state.statement(statement, steps.len()));
        }

        if matches!(
            state.context,
            TransactionContext::ImplicitBatch | TransactionContext::ExtendedBatch
        ) {
            // The end of a Query or batch commits its still-open implicit block.
            let last = steps.last_mut().expect("a segment contains a statement");
            last.effect = TransactionEffect::Commit;
            state.close();
        }
    }

    let sqlx = migration.identity.adapter == MigrationAdapter::Sqlx;
    let end_effect = match state.context {
        TransactionContext::Runner => TransactionEffect::Commit,
        TransactionContext::Explicit
            if migration.execution.transaction_wrapper == TransactionWrapper::File =>
        {
            // The wrapper still sends its final COMMIT after an explicit chained command.
            TransactionEffect::Commit
        }
        TransactionContext::Explicit if !sqlx => TransactionEffect::Rollback,
        TransactionContext::Explicit => {
            if let Some(statement) = migration.statements.last() {
                gaps.push(transaction_gap(
                    statement,
                    "the SQLx migration leaves an explicit transaction open on the runner connection",
                ));
            }
            TransactionEffect::Invalidate
        }
        TransactionContext::Unknown => TransactionEffect::Invalidate,
        TransactionContext::Standalone
        | TransactionContext::ImplicitBatch
        | TransactionContext::ExtendedBatch => TransactionEffect::None,
    };
    let leaves_uncertainty = matches!(
        state.context,
        TransactionContext::Explicit | TransactionContext::Unknown
    ) && !state.stopped;
    (
        TransactionTimeline {
            steps,
            end_effect,
            gaps,
        },
        leaves_uncertainty,
    )
}

struct BlockState<'a> {
    context: TransactionContext,
    pending_start: bool,
    savepoints: Vec<(&'a str, usize)>,
    stopped: bool,
}

impl<'a> BlockState<'a> {
    fn start(&mut self, context: TransactionContext) {
        self.context = context;
        self.pending_start = true;
    }

    fn close(&mut self) {
        self.context = TransactionContext::Standalone;
        self.pending_start = false;
        self.savepoints.clear();
    }

    fn invalidate(&mut self) {
        self.close();
        self.context = TransactionContext::Unknown;
    }

    fn statement(&mut self, statement: &'a Statement, index: usize) -> TransactionStep {
        let context = self.context;
        let mut starts_block = std::mem::take(&mut self.pending_start);
        let result = match (&statement.operation, context) {
            (_, TransactionContext::Unknown) | (Operation::Unsupported, _) => {
                self.invalidate();
                Ok(TransactionEffect::Invalidate)
            }
            (Operation::Transaction(Transaction::Begin), TransactionContext::Standalone) => {
                self.context = TransactionContext::Explicit;
                starts_block = true;
                Ok(TransactionEffect::None)
            }
            (
                Operation::Transaction(Transaction::Begin),
                TransactionContext::ImplicitBatch | TransactionContext::ExtendedBatch,
            ) => {
                // BEGIN promotes the current implicit block, retaining its work and locks.
                self.context = TransactionContext::Explicit;
                Ok(TransactionEffect::None)
            }
            (Operation::Transaction(transaction), _) => self.control(transaction, index),
            (operation, _) if context.shares_transaction() && requires_standalone(operation) => {
                Err(TransactionConflict::ConcurrentIndex)
            }
            _ => Ok(TransactionEffect::None),
        };
        let (effect, conflict) = match result {
            Ok(effect) => (effect, None),
            Err(conflict) => {
                self.invalidate();
                self.stopped = true;
                (TransactionEffect::Invalidate, Some(conflict))
            }
        };
        TransactionStep {
            context,
            starts_block,
            effect,
            conflict,
        }
    }

    fn control(
        &mut self,
        command: &'a Transaction,
        index: usize,
    ) -> Result<TransactionEffect, TransactionConflict> {
        match command {
            // A nested BEGIN warns and leaves the current explicit or runner block open.
            Transaction::Begin => Ok(TransactionEffect::None),
            Transaction::Commit { chain } | Transaction::Rollback { chain } => {
                if *chain && !self.context.explicit() {
                    return Err(TransactionConflict::ChainOutsideBlock);
                }
                let active = self.context.shares_transaction();
                self.close();
                if *chain {
                    self.start(TransactionContext::Explicit);
                }
                Ok(if !active {
                    TransactionEffect::None
                } else if matches!(command, Transaction::Commit { .. }) {
                    TransactionEffect::Commit
                } else {
                    TransactionEffect::Rollback
                })
            }
            Transaction::Savepoint(name) => {
                if !self.context.explicit() {
                    return Err(TransactionConflict::SavepointOutsideBlock);
                }
                self.savepoints.push((name, index));
                Ok(TransactionEffect::Savepoint(index))
            }
            Transaction::RollbackTo(name) | Transaction::Release(name) => {
                if !self.context.explicit() {
                    return Err(TransactionConflict::SavepointOutsideBlock);
                }
                let position = self
                    .savepoints
                    .iter()
                    .rposition(|(existing, _)| *existing == name)
                    .ok_or(TransactionConflict::MissingSavepoint)?;
                let (_, marker) = self.savepoints[position];
                if matches!(command, Transaction::RollbackTo(_)) {
                    self.savepoints.truncate(position + 1);
                    Ok(TransactionEffect::RollbackTo(marker))
                } else {
                    self.savepoints.truncate(position);
                    Ok(TransactionEffect::Release(marker))
                }
            }
        }
    }
}

fn requires_standalone(operation: &Operation) -> bool {
    match operation {
        Operation::CreateIndex(index) => index.concurrently,
        Operation::Drop(drop) => drop.kind == ObjectKind::Index && drop.concurrently,
        _ => false,
    }
}

fn first_missing_statement(migration: &ParsedMigration) -> Option<usize> {
    migration
        .coverage
        .gaps()
        .iter()
        .filter_map(|gap| {
            if !matches!(
                gap.kind,
                CoverageGapKind::UnsupportedInput
                    | CoverageGapKind::UnreadableInput
                    | CoverageGapKind::InputLimitExceeded
                    | CoverageGapKind::ParseError
                    | CoverageGapKind::UnsupportedSql
            ) {
                return None;
            }
            let DiagnosticScope::Span { span } = &gap.scope else {
                return Some(0);
            };
            let position = migration
                .statements
                .partition_point(|stmt| stmt.span.bytes.start() <= span.bytes.start());
            let covered = gap.kind == CoverageGapKind::UnsupportedSql
                && position.checked_sub(1).is_some_and(|index| {
                    span.bytes.end() <= migration.statements[index].span.bytes.end()
                });
            (!covered).then_some(span.bytes.start())
        })
        .min()
}

fn transaction_gap(statement: &Statement, message: &str) -> CoverageGap {
    CoverageGap {
        scope: DiagnosticScope::Span {
            span: statement.span.clone(),
        },
        kind: CoverageGapKind::UnresolvedTransactionMode,
        message: message.into(),
        related_paths: Vec::new(),
    }
}

#[cfg(test)]
mod tests {
    use dmg_core::{
        digest::ContentDigest,
        migration::{
            ExecutionMode, FlywaySettings, MigrationDirection, MigrationFile, MigrationIdentity,
        },
        source::{ByteRange, RepoPath},
        target::PostgresTarget,
    };
    use dmg_postgres::parse_migration;

    use super::*;

    #[test]
    fn unresolved_flyway_wrapper_keeps_supported_statements_in_unknown_context() {
        let bytes = b"ALTER TABLE app.items ADD COLUMN tag text;\nVACUUM app.items;\n";
        let file = MigrationFile {
            identity: MigrationIdentity::flyway_version("1").unwrap(),
            direction: MigrationDirection::Forward,
            path: RepoPath::new("sql/V1__fixture.sql").unwrap(),
            digest: ContentDigest::of(bytes),
            source_segments: vec![ByteRange::new(0, bytes.len()).unwrap()],
            execution: ExecutionMode {
                transaction_wrapper: TransactionWrapper::Flyway(FlywaySettings::default()),
                statement_delivery: StatementDelivery::PerStatement,
            },
        };
        let migration = parse_migration(&file, bytes, PostgresTarget::V18);
        let (timeline, _) = timeline(&migration, false);
        assert!(!timeline.steps.is_empty());
        assert!(
            timeline
                .steps
                .iter()
                .all(|step| step.context == TransactionContext::Unknown)
        );
    }
}
