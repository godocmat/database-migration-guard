use std::time::Instant;

use dmg_core::{
    coverage::CoverageStatus,
    diff::compare_snapshots,
    inspection::InspectionEvidenceV1,
    migration::TransactionWrapper,
    rehearsal::{
        RehearsalBindingV1, RehearsalCleanup, RehearsalConstraintV1, RehearsalDurability,
        RehearsalFailureReason, RehearsalFailureV1, RehearsalMigrationState, RehearsalMigrationV1,
        RehearsalOutcome, RehearsalPhase, RehearsalReportV1, RehearsalSchemaV1,
        RehearsalStatementState,
    },
};
use dmg_inspect::{CatalogDetails, CatalogOptions, InspectionTimeouts, observe_session_catalog};
use dmg_postgres::Transaction;

use crate::{DockerWorker, DockerWorkerConfig, WorkerError, WorkerStream};

use super::{
    failure as report_failure, finish_clock, millis,
    plan::{
        PlannedMigration, PlannedQuery, PreparedRehearsal, PreparedScript, StatementEffect,
        image_digest,
    },
    protocol::{CommandKind, ProtocolError, QueryResult, Ready, Session},
    unix_time,
};

/// Replay a prepared revision in a fresh worker and require verified removal.
/// Runtime errors are represented as typed, sanitized evidence.
pub async fn run_rehearsal(
    config: DockerWorkerConfig,
    plan: PreparedRehearsal<'_>,
) -> RehearsalReportV1 {
    let started = Instant::now();
    let start_time = unix_time();
    let mut report = RehearsalReportV1::new(plan.binding.clone(), start_time.unwrap_or_default());
    report.migrations = plan.expected_migrations();
    let mut progress = Progress::default();

    if start_time.is_none() {
        report.failure = Some(report_failure(
            RehearsalPhase::Clock,
            RehearsalFailureReason::Clock,
        ));
    } else if image_digest(config.image.config_id()) != plan.binding.image_config_digest {
        report.failure = Some(report_failure(
            RehearsalPhase::Provision,
            RehearsalFailureReason::Worker,
        ));
    } else {
        let config = config.with_schemas(plan.binding.scope.clone());
        let result = DockerWorker::run(config, async |worker| {
            progress.worker_started = true;
            observe_run(worker, plan, &mut report, &mut progress).await;
            Ok(())
        })
        .await;
        progress.finish(&mut report, result);
    }

    finish_clock(&mut report);
    report.elapsed_millis = millis(started.elapsed());
    report
}

struct Progress {
    worker_started: bool,
    phase: RehearsalPhase,
    migration: Option<(usize, Instant)>,
    group: Option<(usize, Instant)>,
}

impl Default for Progress {
    fn default() -> Self {
        Self {
            worker_started: false,
            phase: RehearsalPhase::Provision,
            migration: None,
            group: None,
        }
    }
}

impl Progress {
    fn finish(&self, report: &mut RehearsalReportV1, result: Result<(), WorkerError>) {
        if let Some((index, started)) = self.migration {
            let migration = &mut report.migrations[index];
            migration.elapsed_millis = Some(millis(started.elapsed()));
            if let Some((group, started)) = self.group {
                migration.groups[group].elapsed_millis = Some(millis(started.elapsed()));
            }
        }
        match result {
            Ok(()) => report.cleanup = RehearsalCleanup::Verified,
            Err(WorkerError::CleanupUnconfirmed { .. }) => {
                report.cleanup = RehearsalCleanup::Unconfirmed;
                report.outcome = RehearsalOutcome::Incomplete;
                report.failure.get_or_insert_with(|| {
                    report_failure(
                        RehearsalPhase::Cleanup,
                        RehearsalFailureReason::CleanupUnconfirmed,
                    )
                });
            }
            Err(error) => {
                if self.worker_started {
                    report.cleanup = RehearsalCleanup::Verified;
                }
                let reason = match error {
                    WorkerError::Deadline(_) => RehearsalFailureReason::Deadline,
                    WorkerError::UnsupportedPlatform => RehearsalFailureReason::UnsupportedPlatform,
                    WorkerError::RuntimeUnavailable => RehearsalFailureReason::RuntimeUnavailable,
                    _ => RehearsalFailureReason::Worker,
                };
                let path = self
                    .migration
                    .map(|(index, _)| report.migrations[index].path.clone());
                let mut stopped = report_failure(self.phase, reason);
                stopped.path = path;
                record_failure(report, stopped);
            }
        }
    }
}

async fn observe_run(
    worker: &DockerWorker,
    plan: PreparedRehearsal<'_>,
    report: &mut RehearsalReportV1,
    progress: &mut Progress,
) {
    if let Err(error) = setup(worker, &plan, report, progress).await {
        record_failure(report, *error);
        return;
    }

    progress.phase = RehearsalPhase::Before;
    let Some((before, before_summary)) =
        snapshot(worker, &report.binding, report.started_at_unix_seconds).await
    else {
        record_failure(
            report,
            report_failure(progress.phase, RehearsalFailureReason::SnapshotUnavailable),
        );
        return;
    };
    report.before = Some(before_summary);

    progress.phase = RehearsalPhase::Migration;
    for (index, planned) in plan.migrations.into_iter().enumerate() {
        let start = Instant::now();
        progress.migration = Some((index, start));
        let migration = &mut report.migrations[index];
        migration.state = RehearsalMigrationState::Interrupted;
        let result = execute_migration(
            worker,
            &report.binding,
            planned,
            migration,
            &mut progress.group,
        )
        .await;
        migration.elapsed_millis = Some(millis(start.elapsed()));
        progress.migration = None;
        progress.group = None;
        match result {
            Ok(()) => migration.state = RehearsalMigrationState::Completed,
            Err(mut error) => {
                if error.reason == RehearsalFailureReason::Database
                    && error.sqlstate.as_deref() != Some("57014")
                {
                    migration.state = RehearsalMigrationState::Failed;
                }
                error.path = Some(migration.path.clone());
                record_failure(report, *error);
                break;
            }
        }
    }

    progress.phase = RehearsalPhase::After;
    if let Some((after, after_summary)) =
        snapshot(worker, &report.binding, report.started_at_unix_seconds).await
    {
        report.after = Some(after_summary);
        report.schema_diff = compare_snapshots(&before, &after, &[]).ok();
    }
    if report.failure.is_none() {
        if report.schema_diff.is_some() {
            report.outcome = RehearsalOutcome::Succeeded;
        } else {
            record_failure(
                report,
                report_failure(progress.phase, RehearsalFailureReason::SnapshotUnavailable),
            );
        }
    }
}

fn record_failure(report: &mut RehearsalReportV1, error: RehearsalFailureV1) {
    if report.failure.is_some() {
        return;
    }
    report.outcome = match (error.phase, error.reason, error.sqlstate.as_deref()) {
        (_, RehearsalFailureReason::Deadline, _) | (_, _, Some("57014")) => {
            RehearsalOutcome::TimedOut
        }
        (RehearsalPhase::Migration, RehearsalFailureReason::Database, Some(_)) => {
            RehearsalOutcome::Failed
        }
        _ => RehearsalOutcome::Incomplete,
    };
    report.failure = Some(error);
}

async fn setup(
    worker: &DockerWorker,
    plan: &PreparedRehearsal<'_>,
    report: &mut RehearsalReportV1,
    progress: &mut Progress,
) -> ExecutionResult<()> {
    let Some(baseline) = plan.baseline else {
        return Ok(());
    };
    progress.phase = RehearsalPhase::Baseline;
    let start = Instant::now();
    let mut session = connect(worker, &plan.binding, progress.phase).await?;
    let result = run_script(&mut session, baseline, progress.phase).await;
    report.baseline_elapsed_millis = Some(millis(start.elapsed()));
    result?;

    if let Some(seed) = plan.seed {
        progress.phase = RehearsalPhase::Seed;
        let start = Instant::now();
        let result = run_script(&mut session, seed, progress.phase).await;
        report.seed_elapsed_millis = Some(millis(start.elapsed()));
        report.seeded_rows_inserted = Some(result?);
    }
    Ok(())
}

async fn run_script(
    session: &mut Session<WorkerStream<'_>>,
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

async fn snapshot(
    worker: &DockerWorker,
    binding: &RehearsalBindingV1,
    started_at: u64,
) -> Option<(InspectionEvidenceV1, RehearsalSchemaV1)> {
    let options = CatalogOptions::new(u32::from(binding.target.major()))
        .ok()?
        .with_details(CatalogDetails {
            definitions: true,
            total_bytes: false,
        });
    let evidence = worker
        .with_client(async |client| {
            Ok(observe_session_catalog(
                client,
                binding.target_id.clone(),
                &binding.scope,
                options,
                InspectionTimeouts::default(),
            )
            .await)
        })
        .await
        .ok()?
        .ok()?;
    let observation = evidence.observation();
    if observation.server_version_num != Some(binding.server_version_num)
        || observation.observed_at_unix_seconds < started_at
    {
        return None;
    }
    let snapshot = evidence.snapshot()?;
    let coverage = compare_snapshots(&evidence, &evidence, &[])
        .map_or(CoverageStatus::Incomplete, |diff| diff.coverage);
    let summary = RehearsalSchemaV1 {
        structural_digest: snapshot.digest().ok()?,
        server_version_num: binding.server_version_num,
        observed_at_unix_seconds: observation.observed_at_unix_seconds,
        coverage,
        constraints: snapshot
            .tables()
            .iter()
            .flat_map(|table| {
                table
                    .constraints
                    .iter()
                    .map(|constraint| RehearsalConstraintV1 {
                        table: table.name.clone(),
                        name: constraint.name.clone(),
                        kind: constraint.kind,
                        validated: constraint.validated,
                        enforced: constraint.enforced,
                    })
            })
            .collect(),
    };
    Some((evidence, summary))
}

async fn connect<'worker>(
    worker: &'worker DockerWorker,
    binding: &RehearsalBindingV1,
    phase: RehearsalPhase,
) -> ExecutionResult<Session<WorkerStream<'worker>>> {
    let mut session = Session::connect(worker)
        .await
        .map_err(|error| protocol_failure(phase, error))?;
    let path = binding
        .search_path
        .iter()
        .map(|schema| format!("\"{}\"", schema.replace('"', "\"\"")))
        .collect::<Vec<_>>()
        .join(", ");
    let sql = format!("SET search_path TO {path}");
    fixed(&mut session, &sql, phase, Ready::Idle).await?;
    Ok(session)
}

async fn query(
    session: &mut Session<WorkerStream<'_>>,
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
    session: &mut Session<WorkerStream<'_>>,
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

async fn execute_migration(
    worker: &DockerWorker,
    binding: &RehearsalBindingV1,
    planned: PlannedMigration<'_>,
    migration: &mut RehearsalMigrationV1,
    active_group: &mut Option<(usize, Instant)>,
) -> ExecutionResult<()> {
    const PHASE: RehearsalPhase = RehearsalPhase::Migration;
    let mut session = connect(worker, binding, PHASE).await.map_err(|mut error| {
        if error.reason == RehearsalFailureReason::Database {
            error.reason = RehearsalFailureReason::Transport;
        }
        error
    })?;
    let wrapped = migration.execution.transaction_wrapper == TransactionWrapper::File;
    if wrapped {
        fixed(&mut session, "BEGIN", PHASE, Ready::Transaction).await?;
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
        let result = query(&mut session, &group.query, group.effects.len(), PHASE).await;
        migration.groups[index].elapsed_millis = Some(millis(start.elapsed()));
        *active_group = None;

        let result = result?;
        ready = result.ready;
        let error = record_group(migration, &mut pending, index, &group.effects, result)?;
        if let Some(error) = error {
            rollback_open(&mut session, migration, &mut pending, ready).await;
            return Err(error);
        }
    }
    if wrapped {
        let result = query(&mut session, &PlannedQuery::Simple("COMMIT"), 1, PHASE).await?;
        if let Some(sqlstate) = result.sqlstate {
            if result.ready == Ready::Idle {
                pending.finish(migration, RehearsalDurability::RolledBack);
            } else {
                rollback_open(&mut session, migration, &mut pending, result.ready).await;
            }
            return Err(database_failure(PHASE, sqlstate));
        }
        if result.ready != Ready::Idle || result.completed[0].kind != CommandKind::Commit {
            return Err(failure(PHASE, RehearsalFailureReason::OpenTransaction));
        }
        pending.finish(migration, RehearsalDurability::Committed);
    } else if ready != Ready::Idle {
        rollback_open(&mut session, migration, &mut pending, ready).await;
        return Err(failure(PHASE, RehearsalFailureReason::OpenTransaction));
    }
    Ok(())
}

async fn rollback_open(
    session: &mut Session<WorkerStream<'_>>,
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

type ExecutionResult<T> = Result<T, Box<RehearsalFailureV1>>;

fn failure(phase: RehearsalPhase, reason: RehearsalFailureReason) -> Box<RehearsalFailureV1> {
    Box::new(report_failure(phase, reason))
}

fn protocol_failure(phase: RehearsalPhase, error: ProtocolError) -> Box<RehearsalFailureV1> {
    let reason = match error {
        ProtocolError::Database(sqlstate) => return database_failure(phase, sqlstate),
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
