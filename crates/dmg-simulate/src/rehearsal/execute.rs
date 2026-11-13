use std::time::Instant;

use dmg_core::{
    coverage::CoverageStatus,
    diff::compare_snapshots,
    inspection::{InspectionEvidenceV1, InspectionGapReason},
    rehearsal::{
        RehearsalBindingV1, RehearsalCleanup, RehearsalConstraintV1, RehearsalFailureReason,
        RehearsalFailureV1, RehearsalMigrationState, RehearsalMigrationV1, RehearsalOutcome,
        RehearsalPhase, RehearsalReportV1, RehearsalSchemaV1,
    },
};
use dmg_inspect::{CatalogDetails, CatalogOptions, InspectionTimeouts, observe_session_catalog};

use crate::{DockerWorker, DockerWorkerConfig, WorkerError, WorkerStream};

use super::{
    failure as report_failure, finish_clock, millis,
    plan::{PlannedMigration, PreparedRehearsal, image_digest},
    protocol::Session,
    runner::{
        ExecutionResult, execute_migration, initialize_session, protocol_failure, run_script,
    },
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
        let mut stopped_at = None;
        let result = DockerWorker::run(config, async |worker| {
            let _stop = StopTime(&mut stopped_at);
            progress.worker_started = true;
            observe_run(worker, plan, &mut report, &mut progress).await;
            Ok(())
        })
        .await;
        progress.finish(&mut report, result, stopped_at);
    }

    finish_clock(&mut report);
    report.elapsed_millis = millis(started.elapsed());
    report
}

struct StopTime<'a>(&'a mut Option<Instant>);

impl Drop for StopTime<'_> {
    fn drop(&mut self) {
        *self.0 = Some(Instant::now());
    }
}

struct Progress {
    worker_started: bool,
    phase: RehearsalPhase,
    setup: Option<Instant>,
    migration: Option<(usize, Instant)>,
    group: Option<(usize, Instant)>,
}

impl Default for Progress {
    fn default() -> Self {
        Self {
            worker_started: false,
            phase: RehearsalPhase::Provision,
            setup: None,
            migration: None,
            group: None,
        }
    }
}

impl Progress {
    fn finish(
        &self,
        report: &mut RehearsalReportV1,
        result: Result<(), WorkerError>,
        stopped_at: Option<Instant>,
    ) {
        let elapsed = |started| {
            millis(
                stopped_at
                    .unwrap_or_else(Instant::now)
                    .duration_since(started),
            )
        };
        if let Some(started) = self.setup {
            let timing = if self.phase == RehearsalPhase::Seed {
                &mut report.seed_elapsed_millis
            } else {
                &mut report.baseline_elapsed_millis
            };
            *timing = Some(elapsed(started));
        }
        if let Some((index, started)) = self.migration {
            let migration = &mut report.migrations[index];
            migration.elapsed_millis = Some(elapsed(started));
            if let Some((group, started)) = self.group {
                migration.groups[group].elapsed_millis = Some(elapsed(started));
            }
        }
        match result {
            Ok(()) => report.cleanup = RehearsalCleanup::Verified,
            Err(WorkerError::CleanupUnconfirmed { worker, .. }) => {
                report.cleanup = RehearsalCleanup::Unconfirmed;
                report.cleanup_worker = Some(worker);
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
    let (before, before_summary) =
        match snapshot(worker, &report.binding, report.started_at_unix_seconds).await {
            Ok(observation) => observation,
            Err(reason) => {
                record_failure(report, report_failure(progress.phase, reason));
                return;
            }
        };
    let observed_at = before_summary.observed_at_unix_seconds;
    report.before = Some(before_summary);

    progress.phase = RehearsalPhase::Migration;
    for (index, planned) in plan.migrations.into_iter().enumerate() {
        let start = Instant::now();
        progress.migration = Some((index, start));
        let migration = &mut report.migrations[index];
        migration.state = RehearsalMigrationState::Interrupted;
        let result = execute_on_worker(
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
    let after = match snapshot(worker, &report.binding, observed_at).await {
        Ok((after, summary)) => {
            report.after = Some(summary);
            after
        }
        Err(reason) => {
            record_failure(report, report_failure(progress.phase, reason));
            return;
        }
    };
    report.schema_diff = compare_snapshots(&before, &after, &[]).ok();
    if report.schema_diff.is_none() {
        record_failure(
            report,
            report_failure(progress.phase, RehearsalFailureReason::SnapshotUnavailable),
        );
    } else if report.failure.is_none() {
        report.outcome = RehearsalOutcome::Succeeded;
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
    progress.setup = Some(start);
    let mut session = connect(worker, &plan.binding, progress.phase).await?;
    let result = run_script(&mut session, baseline, progress.phase).await;
    report.baseline_elapsed_millis = Some(millis(start.elapsed()));
    progress.setup = None;
    result?;

    if let Some(seed) = plan.seed {
        progress.phase = RehearsalPhase::Seed;
        let start = Instant::now();
        progress.setup = Some(start);
        let result = run_script(&mut session, seed, progress.phase).await;
        report.seed_elapsed_millis = Some(millis(start.elapsed()));
        progress.setup = None;
        report.seeded_rows_inserted = Some(result?);
    }
    Ok(())
}

async fn snapshot(
    worker: &DockerWorker,
    binding: &RehearsalBindingV1,
    earliest: u64,
) -> Result<(InspectionEvidenceV1, RehearsalSchemaV1), RehearsalFailureReason> {
    let options = CatalogOptions::new(u32::from(binding.target.major()))
        .map_err(|_| RehearsalFailureReason::SnapshotUnavailable)?
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
        .map_err(|error| match error {
            WorkerError::Deadline(_) => RehearsalFailureReason::Deadline,
            _ => RehearsalFailureReason::SnapshotUnavailable,
        })?
        .map_err(|error| match error {
            dmg_inspect::CatalogError::Clock => RehearsalFailureReason::Clock,
            _ => RehearsalFailureReason::SnapshotUnavailable,
        })?;
    let observation = evidence.observation();
    if observation.observed_at_unix_seconds < earliest {
        return Err(RehearsalFailureReason::Clock);
    }
    let unavailable = || {
        if evidence
            .gaps()
            .iter()
            .any(|gap| gap.reason == InspectionGapReason::Deadline)
        {
            RehearsalFailureReason::Deadline
        } else {
            RehearsalFailureReason::SnapshotUnavailable
        }
    };
    if observation.server_version_num != Some(binding.server_version_num) {
        return Err(unavailable());
    }
    let snapshot = evidence.snapshot().ok_or_else(unavailable)?;
    let coverage = compare_snapshots(&evidence, &evidence, &[])
        .map_or(CoverageStatus::Incomplete, |diff| diff.coverage);
    let summary = RehearsalSchemaV1 {
        structural_digest: snapshot
            .digest()
            .map_err(|_| RehearsalFailureReason::SnapshotUnavailable)?,
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
    Ok((evidence, summary))
}

async fn connect<'worker>(
    worker: &'worker DockerWorker,
    binding: &RehearsalBindingV1,
    phase: RehearsalPhase,
) -> ExecutionResult<Session<WorkerStream<'worker>>> {
    let mut session = Session::connect(worker)
        .await
        .map_err(|error| protocol_failure(phase, error))?;
    initialize_session(&mut session, &binding.search_path, phase).await?;
    Ok(session)
}

async fn execute_on_worker(
    worker: &DockerWorker,
    binding: &RehearsalBindingV1,
    planned: PlannedMigration<'_>,
    migration: &mut RehearsalMigrationV1,
    active_group: &mut Option<(usize, Instant)>,
) -> ExecutionResult<()> {
    let mut session = connect(worker, binding, RehearsalPhase::Migration)
        .await
        .map_err(|mut error| {
            if error.reason == RehearsalFailureReason::Database {
                error.reason = RehearsalFailureReason::Transport;
            }
            error
        })?;
    execute_migration(&mut session, planned, migration, active_group).await
}
