mod plan;

#[cfg(all(target_os = "linux", target_arch = "x86_64"))]
mod execute;
#[cfg(all(target_os = "linux", target_arch = "x86_64"))]
mod protocol;

pub use plan::{
    MAX_REHEARSAL_SETUP_BYTES, PreparedRehearsal, RehearsalContext, RehearsalInput,
    RehearsalPlanError, expected_rehearsal_plan, prepare_rehearsal,
};

#[cfg(all(target_os = "linux", target_arch = "x86_64"))]
pub use execute::run_rehearsal;

use std::time::{Duration, SystemTime, UNIX_EPOCH};

use dmg_core::rehearsal::{
    RehearsalFailureReason, RehearsalFailureV1, RehearsalPhase, RehearsalReportV1,
};

fn unix_time() -> Option<u64> {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .ok()
        .map(|time| time.as_secs())
}

fn millis(duration: Duration) -> u64 {
    duration.as_millis().try_into().unwrap_or(u64::MAX)
}

fn finish_clock(report: &mut RehearsalReportV1) {
    if let Some(time) = unix_time().filter(|time| {
        *time >= report.started_at_unix_seconds
            && report
                .before
                .iter()
                .chain(&report.after)
                .all(|summary| *time >= summary.observed_at_unix_seconds)
    }) {
        report.completed_at_unix_seconds = Some(time);
    } else {
        report.outcome = dmg_core::rehearsal::RehearsalOutcome::Incomplete;
        report
            .failure
            .get_or_insert_with(|| failure(RehearsalPhase::Clock, RehearsalFailureReason::Clock));
    }
}

fn failure(phase: RehearsalPhase, reason: RehearsalFailureReason) -> RehearsalFailureV1 {
    RehearsalFailureV1 {
        phase,
        reason,
        sqlstate: None,
        path: None,
        span: None,
    }
}

#[cfg(not(all(target_os = "linux", target_arch = "x86_64")))]
/// Return typed incomplete evidence on hosts without the isolated worker.
pub async fn run_rehearsal(
    _config: crate::DockerWorkerConfig,
    plan: PreparedRehearsal<'_>,
) -> RehearsalReportV1 {
    let start = std::time::Instant::now();
    let mut report =
        RehearsalReportV1::new(plan.binding().clone(), unix_time().unwrap_or_default());
    report.migrations = plan.expected_migrations();
    report.failure = Some(failure(
        RehearsalPhase::Provision,
        RehearsalFailureReason::UnsupportedPlatform,
    ));
    finish_clock(&mut report);
    report.elapsed_millis = millis(start.elapsed());
    report
}

#[cfg(test)]
mod tests;
