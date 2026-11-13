use std::{
    fmt,
    io::{self, Write},
};

use dmg_core::{
    migration::{StatementDelivery, TransactionWrapper},
    rehearsal::{
        RehearsalBindingV1, RehearsalCleanup, RehearsalDurability, RehearsalFailureReason,
        RehearsalFailureV1, RehearsalMigrationState, RehearsalMode, RehearsalPhase,
        RehearsalReportV1, RehearsalSchemaV1, RehearsalScriptV1, RehearsalStatementState, SeedKind,
    },
    snapshot::ConstraintKind,
};

use super::{
    EvidenceFormat, Terminal, coverage_name,
    diff::{Identifier, Qualified, write_comparison},
    evidence::Review,
    rehearsal_outcome_name,
};

/// Render the recorded result of a disposable PostgreSQL rehearsal.
pub fn write_rehearsal_review(
    report: &RehearsalReportV1,
    format: EvidenceFormat,
    output: impl Write,
) -> io::Result<()> {
    let mut out = Review::new(output, Some(format));
    out.start("Migration rehearsal")?;
    out.line(format_args!(
        "Outcome {} · Cleanup {} · Elapsed {} ms",
        rehearsal_outcome_name(report.outcome),
        cleanup_name(report.cleanup),
        report.elapsed_millis,
    ))?;
    if let Some(worker) = &report.cleanup_worker {
        out.detail(format_args!("Worker for cleanup {}", Terminal(worker)))?;
    }
    if let Some(failure) = &report.failure {
        write_failure(&mut out, failure)?;
    }
    write_binding(&mut out, &report.binding)?;

    out.section("Observations")?;
    out.detail(format_args!(
        "Started at Unix time {} · Completed at Unix time {}",
        report.started_at_unix_seconds,
        ObservationValue(report.completed_at_unix_seconds, ""),
    ))?;
    if !matches!(report.binding.mode, RehearsalMode::EmptyDatabase) {
        out.detail(format_args!(
            "Baseline elapsed {}",
            ObservationValue(report.baseline_elapsed_millis, " ms"),
        ))?;
    }
    if matches!(report.binding.mode, RehearsalMode::Seeded { .. }) {
        out.detail(format_args!(
            "Seed elapsed {} · Rows inserted {}",
            ObservationValue(report.seed_elapsed_millis, " ms"),
            ObservationValue(report.seeded_rows_inserted, ""),
        ))?;
    }

    write_schema(&mut out, "Before migration", report.before.as_ref())?;
    write_schema(&mut out, "After migration", report.after.as_ref())?;
    if let Some(diff) = &report.schema_diff {
        write_comparison("Schema delta", diff, &mut out)?;
    } else {
        out.line(format_args!("Schema delta unavailable."))?;
    }

    out.section("Migrations")?;
    let mut migration_counts = [0; 4];
    for migration in &report.migrations {
        let index = match migration.state {
            RehearsalMigrationState::Completed => 0,
            RehearsalMigrationState::Failed => 1,
            RehearsalMigrationState::Interrupted => 2,
            RehearsalMigrationState::Skipped => 3,
        };
        migration_counts[index] += 1;
    }
    let [completed, failed, interrupted, skipped] = migration_counts;
    out.line(format_args!(
        "{} forward migrations · {completed} completed · {failed} failed · {interrupted} interrupted · {skipped} skipped",
        report.migrations.len(),
    ))?;
    out.details(report.migrations.iter(), |out, migration| {
        let wrapper = match migration.execution.transaction_wrapper {
            TransactionWrapper::File => "file transaction",
            TransactionWrapper::None => "no transaction wrapper",
            TransactionWrapper::Flyway(_) => "Flyway transaction settings",
        };
        out.detail(format_args!(
            "Migration {} · {} · {} · Elapsed {}",
            Terminal(migration.path.as_str()),
            migration_name(migration.state),
            wrapper,
            ObservationValue(migration.elapsed_millis, " ms"),
        ))
    })?;

    // A single cap covers all migration groups and all statements, independent
    // of the number of migrations, so a saved artifact cannot multiply the output.
    let statements = report
        .migrations
        .iter()
        .flat_map(|migration| &migration.groups)
        .flat_map(|group| {
            group
                .statements
                .iter()
                .map(move |statement| (group, statement))
        })
        .filter(|(_, statement)| statement.state != RehearsalStatementState::Skipped);
    let mut durability_counts = [0; 4];
    for (_, statement) in statements.clone() {
        let index = match statement.durability {
            Some(RehearsalDurability::Committed) => 0,
            Some(RehearsalDurability::RolledBack) => 1,
            Some(RehearsalDurability::Unknown) => 2,
            None => 3,
        };
        durability_counts[index] += 1;
    }
    if durability_counts.iter().all(|&count| count == 0) {
        return out.finish();
    }
    let [committed, rolled_back, unknown, unobserved] = durability_counts;
    out.section("Statement observations")?;
    out.line(format_args!(
        "Durability · {committed} committed · {rolled_back} rolled back · {unknown} unknown · {unobserved} unobserved",
    ))?;
    out.line(format_args!(
        "Statement elapsed time starts at group dispatch."
    ))?;
    out.details(statements, |out, (group, statement)| {
        let durability = match statement.durability {
            Some(RehearsalDurability::Committed) => "committed",
            Some(RehearsalDurability::RolledBack) => "rolled back",
            Some(RehearsalDurability::Unknown) => "unknown",
            None => "unobserved",
        };
        out.detail(format_args!(
            "{} · {} · Durability {durability} · {} · Group elapsed {} · Statement elapsed {}",
            super::source_label(&statement.span, 240),
            statement_name(statement.state),
            delivery_name(group.delivery),
            ObservationValue(group.elapsed_millis, " ms"),
            ObservationValue(statement.elapsed_millis, " ms"),
        ))
    })?;
    out.finish()
}

fn write_failure(out: &mut Review<impl Write>, failure: &RehearsalFailureV1) -> io::Result<()> {
    out.section("Recorded failure")?;
    out.line(format_args!(
        "{} · {}",
        phase_name(failure.phase),
        failure_name(failure.reason),
    ))?;
    if let Some(sqlstate) = &failure.sqlstate {
        out.detail(format_args!("SQLSTATE {}", Terminal(sqlstate)))?;
    }
    if let Some(span) = &failure.span {
        out.detail(format_args!("Source {}", super::source_label(span, 240)))?;
    } else if let Some(path) = &failure.path {
        out.detail(format_args!("Source {}", Terminal(path.as_str())))?;
    }
    Ok(())
}

fn write_binding(out: &mut Review<impl Write>, binding: &RehearsalBindingV1) -> io::Result<()> {
    out.section("Rehearsal binding")?;
    out.detail(format_args!(
        "Repository {}",
        binding.repository_id.as_str()
    ))?;
    out.detail(format_args!("Target {}", binding.target_id.as_str()))?;
    out.detail(format_args!(
        "PostgreSQL pin {}.{} · {} profile v{}",
        binding.target,
        binding.server_version_num % 10_000,
        binding.adapter.as_str(),
        binding.profile_version,
    ))?;
    if let Ok(digest) = binding.digest() {
        out.detail(format_args!("Rehearsal binding SHA-256 {digest}"))?;
    }
    out.detail(format_args!("Git base {}", Terminal(&binding.base_commit)))?;
    out.detail(format_args!("Git head {}", Terminal(&binding.head_commit)))?;
    for (label, digest) in [
        ("Project configuration", binding.project_config_digest),
        ("Policy", binding.policy_digest),
        ("Forward inventory", binding.forward_inventory_digest),
        ("Execution plan", binding.execution_plan_digest),
        ("Image manifest", binding.image_manifest_digest),
        ("Image configuration", binding.image_config_digest),
    ] {
        out.detail(format_args!("{label} SHA-256 {digest}"))?;
    }
    match &binding.mode {
        RehearsalMode::EmptyDatabase => out.detail(format_args!("Mode empty database"))?,
        RehearsalMode::SchemaOnly { baseline } => {
            out.detail(format_args!("Mode schema-only"))?;
            write_script(out, "Baseline", baseline)?;
        }
        RehearsalMode::Seeded {
            baseline,
            seed,
            seed_kind,
        } => {
            let seed_kind = match seed_kind {
                SeedKind::Synthetic => "synthetic",
                SeedKind::Sanitized => "sanitized",
            };
            out.detail(format_args!("Mode seeded · {seed_kind} seed"))?;
            write_script(out, "Baseline", baseline)?;
            write_script(out, "Seed", seed)?;
        }
    }
    out.detail(format_args!(
        "Authorized schemas {}",
        SchemaNames(binding.scope.schemas()),
    ))?;
    out.detail(format_args!(
        "Search path {}",
        SchemaNames(&binding.search_path)
    ))?;
    Ok(())
}

fn write_script(
    out: &mut Review<impl Write>,
    label: &'static str,
    script: &RehearsalScriptV1,
) -> io::Result<()> {
    out.detail(format_args!(
        "{label} SHA-256 {} · {} bytes",
        script.digest, script.bytes
    ))
}

fn write_schema(
    out: &mut Review<impl Write>,
    label: &'static str,
    summary: Option<&RehearsalSchemaV1>,
) -> io::Result<()> {
    out.section(label)?;
    let Some(summary) = summary else {
        return out.line(format_args!("Schema observation unavailable."));
    };
    out.line(format_args!(
        "{} coverage · {} observed constraints · Unix time {}",
        coverage_name(summary.coverage),
        summary.constraints.len(),
        summary.observed_at_unix_seconds,
    ))?;
    out.detail(format_args!(
        "Structural SHA-256 {}",
        summary.structural_digest
    ))?;
    out.details(summary.constraints.iter(), |out, constraint| {
        out.detail(format_args!(
            "{}.{} · {} · {} · {}",
            Qualified(&constraint.table),
            Identifier(&constraint.name),
            constraint_name(constraint.kind),
            if constraint.validated {
                "validated"
            } else {
                "unvalidated"
            },
            if constraint.enforced {
                "enforced"
            } else {
                "unenforced"
            },
        ))
    })
}

fn cleanup_name(cleanup: RehearsalCleanup) -> &'static str {
    match cleanup {
        RehearsalCleanup::NotObserved => "unobserved",
        RehearsalCleanup::Verified => "verified",
        RehearsalCleanup::Unconfirmed => "unconfirmed",
    }
}

fn phase_name(phase: RehearsalPhase) -> &'static str {
    match phase {
        RehearsalPhase::Provision => "Worker provisioning",
        RehearsalPhase::Baseline => "Baseline setup",
        RehearsalPhase::Seed => "Seed setup",
        RehearsalPhase::Before => "Before migration",
        RehearsalPhase::Migration => "Migration execution",
        RehearsalPhase::After => "After migration",
        RehearsalPhase::Cleanup => "Worker cleanup",
        RehearsalPhase::Clock => "Clock observation",
    }
}

fn failure_name(reason: RehearsalFailureReason) -> &'static str {
    match reason {
        RehearsalFailureReason::UnsupportedPlatform => "Unsupported platform",
        RehearsalFailureReason::RuntimeUnavailable => "Runtime unavailable",
        RehearsalFailureReason::Worker => "Worker failure",
        RehearsalFailureReason::Transport => "PostgreSQL transport failure",
        RehearsalFailureReason::ProtocolLimit => "PostgreSQL protocol limit",
        RehearsalFailureReason::Database => "PostgreSQL rejected the operation",
        RehearsalFailureReason::Deadline => "Deadline expired",
        RehearsalFailureReason::OpenTransaction => "Transaction left open",
        RehearsalFailureReason::SnapshotUnavailable => "Schema snapshot unavailable",
        RehearsalFailureReason::CleanupUnconfirmed => "Cleanup unconfirmed",
        RehearsalFailureReason::Clock => "Clock unavailable",
    }
}

fn migration_name(state: RehearsalMigrationState) -> &'static str {
    match state {
        RehearsalMigrationState::Skipped => "skipped",
        RehearsalMigrationState::Completed => "completed",
        RehearsalMigrationState::Failed => "failed",
        RehearsalMigrationState::Interrupted => "interrupted",
    }
}

fn statement_name(state: RehearsalStatementState) -> &'static str {
    match state {
        RehearsalStatementState::Skipped => "skipped",
        RehearsalStatementState::Completed => "completed",
        RehearsalStatementState::Failed => "failed",
        RehearsalStatementState::Unknown => "unknown",
    }
}

fn delivery_name(delivery: StatementDelivery) -> &'static str {
    match delivery {
        StatementDelivery::PerStatement => "per-statement query",
        StatementDelivery::SingleQuery => "single query",
        StatementDelivery::ExtendedBatch => "extended batch",
    }
}

fn constraint_name(kind: ConstraintKind) -> &'static str {
    match kind {
        ConstraintKind::Check => "CHECK",
        ConstraintKind::NotNull => "NOT NULL",
        ConstraintKind::PrimaryKey => "PRIMARY KEY",
        ConstraintKind::Unique => "UNIQUE",
        ConstraintKind::ForeignKey => "FOREIGN KEY",
        ConstraintKind::Exclusion => "EXCLUDE",
    }
}

struct ObservationValue(Option<u64>, &'static str);

impl fmt::Display for ObservationValue {
    fn fmt(&self, out: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.0 {
            Some(value) => write!(out, "{value}{}", self.1),
            None => out.write_str("unobserved"),
        }
    }
}

struct SchemaNames<'a>(&'a [String]);

impl fmt::Display for SchemaNames<'_> {
    fn fmt(&self, out: &mut fmt::Formatter<'_>) -> fmt::Result {
        for (index, schema) in self.0.iter().enumerate() {
            if index > 0 {
                out.write_str(", ")?;
            }
            write!(out, "{}", Identifier(schema))?;
        }
        Ok(())
    }
}
