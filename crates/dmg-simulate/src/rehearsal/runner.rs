use std::time::Instant;

use dmg_core::{
    migration::TransactionWrapper,
    rehearsal::{
        RehearsalDurability, RehearsalFailureReason, RehearsalFailureV1, RehearsalMigrationV1,
        RehearsalPhase, RehearsalStatementState,
    },
};
use dmg_postgres::Transaction;
use tokio::io::{AsyncRead, AsyncWrite};

use super::{
    failure as report_failure, millis,
    plan::{PlannedMigration, PlannedQuery, PreparedScript, StatementEffect},
    protocol::{CommandKind, ProtocolError, QueryResult, Ready, Session},
};

pub(super) async fn initialize_session(
    session: &mut Session<impl AsyncRead + AsyncWrite + Unpin>,
    search_path: &[String],
    phase: RehearsalPhase,
) -> ExecutionResult<()> {
    let path = search_path
        .iter()
        .map(|schema| format!("\"{}\"", schema.replace('"', "\"\"")))
        .collect::<Vec<_>>()
        .join(", ");
    let sql = format!("SET search_path TO {path}");
    fixed(session, &sql, phase, Ready::Idle).await?;
    Ok(())
}

pub(super) async fn run_script(
    session: &mut Session<impl AsyncRead + AsyncWrite + Unpin>,
    script: PreparedScript<'_>,
    phase: RehearsalPhase,
) -> ExecutionResult<u64> {
    let result = query(
        session,
        &PlannedQuery::Simple(script.source),
        script.statement_count,
        phase,
    )
    .await?;
    if let Some(sqlstate) = result.sqlstate {
        return Err(database_failure(phase, sqlstate));
    }
    if result.ready != Ready::Idle {
        return Err(failure(phase, RehearsalFailureReason::OpenTransaction));
    }
    result
        .completed
        .into_iter()
        .try_fold(0u64, |sum, command| match command.kind {
            CommandKind::Insert(rows) => sum.checked_add(rows),
            _ if phase == RehearsalPhase::Baseline => Some(sum),
            _ => None,
        })
        .ok_or_else(|| failure(phase, RehearsalFailureReason::Transport))
}

async fn query(
    session: &mut Session<impl AsyncRead + AsyncWrite + Unpin>,
    query: &PlannedQuery<'_>,
    count: usize,
    phase: RehearsalPhase,
) -> ExecutionResult<QueryResult> {
    session
        .query(query, count)
        .await
        .map_err(|error| protocol_failure(phase, error))
}

async fn fixed(
    session: &mut Session<impl AsyncRead + AsyncWrite + Unpin>,
    sql: &str,
    phase: RehearsalPhase,
    ready: Ready,
) -> ExecutionResult<CommandKind> {
    let result = query(session, &PlannedQuery::Simple(sql), 1, phase).await?;
    if let Some(sqlstate) = result.sqlstate {
        return Err(database_failure(phase, sqlstate));
    }
    if result.ready != ready {
        return Err(failure(phase, RehearsalFailureReason::OpenTransaction));
    }
    Ok(result.completed[0].kind)
}

pub(super) async fn execute_migration(
    session: &mut Session<impl AsyncRead + AsyncWrite + Unpin>,
    planned: PlannedMigration<'_>,
    migration: &mut RehearsalMigrationV1,
    active_group: &mut Option<(usize, Instant)>,
) -> ExecutionResult<()> {
    const PHASE: RehearsalPhase = RehearsalPhase::Migration;
    let wrapped = migration.execution.transaction_wrapper == TransactionWrapper::File;
    if wrapped {
        fixed(session, "BEGIN", PHASE, Ready::Transaction).await?;
    }
    let mut pending = Pending::default();
    let mut ready = if wrapped {
        Ready::Transaction
    } else {
        Ready::Idle
    };
    for (index, group) in planned.groups.into_iter().enumerate() {
        let start = Instant::now();
        *active_group = Some((index, start));
        for statement in &mut migration.groups[index].statements {
            statement.state = RehearsalStatementState::Unknown;
            statement.durability = Some(RehearsalDurability::Unknown);
        }
        let result = query(session, &group.query, group.effects.len(), PHASE).await;
        migration.groups[index].elapsed_millis = Some(millis(start.elapsed()));
        *active_group = None;

        let result = result?;
        ready = result.ready;
        let error = record_group(migration, &mut pending, index, &group.effects, result)?;
        if let Some(error) = error {
            rollback_open(session, migration, &mut pending, ready).await;
            return Err(error);
        }
    }
    if wrapped {
        let result = query(session, &PlannedQuery::Simple("COMMIT"), 1, PHASE).await?;
        if let Some(sqlstate) = result.sqlstate {
            if result.ready == Ready::Idle {
                pending.finish(migration, RehearsalDurability::RolledBack);
            } else {
                rollback_open(session, migration, &mut pending, result.ready).await;
            }
            return Err(database_failure(PHASE, sqlstate));
        }
        if result.ready != Ready::Idle || result.completed[0].kind != CommandKind::Commit {
            return Err(failure(PHASE, RehearsalFailureReason::OpenTransaction));
        }
        pending.finish(migration, RehearsalDurability::Committed);
    } else if ready != Ready::Idle {
        rollback_open(session, migration, &mut pending, ready).await;
        return Err(failure(PHASE, RehearsalFailureReason::OpenTransaction));
    }
    Ok(())
}

async fn rollback_open(
    session: &mut Session<impl AsyncRead + AsyncWrite + Unpin>,
    migration: &mut RehearsalMigrationV1,
    pending: &mut Pending,
    ready: Ready,
) {
    if ready != Ready::Idle
        && matches!(
            fixed(session, "ROLLBACK", RehearsalPhase::Migration, Ready::Idle).await,
            Ok(CommandKind::Rollback)
        )
    {
        pending.finish(migration, RehearsalDurability::RolledBack);
    }
}

fn record_group(
    migration: &mut RehearsalMigrationV1,
    pending: &mut Pending,
    group: usize,
    effects: &[StatementEffect],
    result: QueryResult,
) -> ExecutionResult<Option<Box<RehearsalFailureV1>>> {
    const PHASE: RehearsalPhase = RehearsalPhase::Migration;
    let completed = result.completed.len();
    for (index, command) in result.completed.into_iter().enumerate() {
        let statement = &mut migration.groups[group].statements[index];
        statement.state = RehearsalStatementState::Completed;
        statement.elapsed_millis = Some(millis(command.elapsed));
        match &effects[index] {
            StatementEffect::Other | StatementEffect::ConcurrentIndex => {
                pending.statements.push((group, index))
            }
            StatementEffect::Transaction(effect) => {
                statement.durability = None;
                if !pending.transaction(migration, effect, command.kind) {
                    return Err(failure(PHASE, RehearsalFailureReason::Transport));
                }
            }
        }
    }

    let error = result.sqlstate.map(|sqlstate| {
        let mut error = database_failure(PHASE, sqlstate);
        if let Some((failed, remaining)) =
            migration.groups[group].statements[completed..].split_first_mut()
        {
            failed.state = RehearsalStatementState::Failed;
            failed.durability = matches!(effects[completed], StatementEffect::ConcurrentIndex)
                .then_some(RehearsalDurability::Unknown);
            error.span = Some(failed.span.clone());
            for statement in remaining {
                statement.state = RehearsalStatementState::Skipped;
                statement.durability = None;
            }
        }
        error
    });

    if result.ready == Ready::Idle {
        let durability = if error.is_some() {
            RehearsalDurability::RolledBack
        } else {
            RehearsalDurability::Committed
        };
        pending.finish(migration, durability);
    }
    Ok(error)
}

#[derive(Default)]
struct Pending {
    statements: Vec<(usize, usize)>,
    savepoints: Vec<(String, usize)>,
}

impl Pending {
    fn transaction(
        &mut self,
        migration: &mut RehearsalMigrationV1,
        transaction: &Transaction,
        command: CommandKind,
    ) -> bool {
        match transaction {
            Transaction::Begin => {}
            Transaction::Commit { .. } => match command {
                CommandKind::Commit => self.finish(migration, RehearsalDurability::Committed),
                CommandKind::Rollback => self.finish(migration, RehearsalDurability::RolledBack),
                _ => return false,
            },
            Transaction::Rollback { .. } => self.finish(migration, RehearsalDurability::RolledBack),
            Transaction::Savepoint(name) => {
                self.savepoints.push((name.clone(), self.statements.len()))
            }
            Transaction::RollbackTo(name) | Transaction::Release(name) => {
                let Some(index) = self.savepoints.iter().rposition(|(point, _)| point == name)
                else {
                    return false;
                };
                if matches!(transaction, Transaction::RollbackTo(_)) {
                    self.resolve_from(
                        migration,
                        self.savepoints[index].1,
                        RehearsalDurability::RolledBack,
                    );
                    self.savepoints.truncate(index + 1);
                } else {
                    self.savepoints.truncate(index);
                }
            }
        }
        true
    }

    fn finish(&mut self, migration: &mut RehearsalMigrationV1, durability: RehearsalDurability) {
        self.resolve_from(migration, 0, durability);
        self.savepoints.clear();
    }

    fn resolve_from(
        &mut self,
        migration: &mut RehearsalMigrationV1,
        offset: usize,
        durability: RehearsalDurability,
    ) {
        for (group, index) in self.statements.drain(offset..) {
            migration.groups[group].statements[index].durability = Some(durability);
        }
    }
}

pub(super) type ExecutionResult<T> = Result<T, Box<RehearsalFailureV1>>;

fn failure(phase: RehearsalPhase, reason: RehearsalFailureReason) -> Box<RehearsalFailureV1> {
    Box::new(report_failure(phase, reason))
}

pub(super) fn protocol_failure(
    phase: RehearsalPhase,
    error: ProtocolError,
) -> Box<RehearsalFailureV1> {
    let reason = match error {
        ProtocolError::Database(sqlstate) => return database_failure(phase, sqlstate),
        #[cfg(all(target_os = "linux", target_arch = "x86_64"))]
        ProtocolError::Deadline => RehearsalFailureReason::Deadline,
        ProtocolError::Limit => RehearsalFailureReason::ProtocolLimit,
        ProtocolError::Transport | ProtocolError::Invalid => RehearsalFailureReason::Transport,
    };
    failure(phase, reason)
}

fn database_failure(phase: RehearsalPhase, sqlstate: String) -> Box<RehearsalFailureV1> {
    let mut error = failure(phase, RehearsalFailureReason::Database);
    error.sqlstate = Some(sqlstate);
    error
}
