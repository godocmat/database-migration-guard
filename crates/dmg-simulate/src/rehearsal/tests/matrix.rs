use std::{env, path::PathBuf};

use dmg_core::{
    diff::StructuralChange,
    migration::{FlywaySettings, TransactionWrapper},
    rehearsal::{
        RehearsalCleanup, RehearsalDurability, RehearsalFailureReason, RehearsalMigrationState,
        RehearsalOutcome, RehearsalPhase, RehearsalReportV1, RehearsalStatementState,
    },
    risk::ObjectKind,
};
use dmg_discovery::SqlxLayout;

use super::*;
use crate::{DockerWorker, DockerWorkerConfig, WorkerLimits, run_rehearsal};

const BASELINE: &[u8] = b"CREATE TABLE items (id integer PRIMARY KEY, note text);";
const SEED: &[u8] = b"INSERT INTO items VALUES (1, 'protected-seed-marker'), (2, NULL);";
const DUPLICATE: &str =
    "ALTER TABLE items ADD COLUMN added integer;\nINSERT INTO items VALUES (1);\nSELECT 9;";
const ALTER_AND_INDEX: &str = "ALTER TABLE items ADD COLUMN added integer;\nCREATE INDEX CONCURRENTLY item_note_idx ON items (note);";
const EXPLICIT: &str = "BEGIN;\nALTER TABLE items ADD COLUMN durable integer;\nSAVEPOINT before_add;\nALTER TABLE items ADD COLUMN reverted integer;\nROLLBACK TO before_add;\nRELEASE before_add;\nCOMMIT;\nBEGIN;\nALTER TABLE items ADD COLUMN aborted integer;\nINSERT INTO items VALUES (1);";

struct Fixture {
    executable: PathBuf,
    socket: PathBuf,
    image: PostgresImage,
    target: PostgresTarget,
}

impl Fixture {
    fn selected() -> Self {
        let (image, target) = match env::var("DMG_WORKER_TEST_POSTGRES").as_deref() {
            Ok("15.20") => (PostgresImage::V15_20, PostgresTarget::V15),
            Ok("16.16") => (PostgresImage::V16_16, PostgresTarget::V16),
            Ok("17.12") => (PostgresImage::V17_12, PostgresTarget::V17),
            Ok("18.7") => (PostgresImage::V18_7, PostgresTarget::V18),
            _ => panic!("fixture PostgreSQL release"),
        };
        Self {
            executable: env::var_os("DMG_WORKER_TEST_DOCKER")
                .expect("fixture Docker executable")
                .into(),
            socket: env::var_os("DMG_WORKER_TEST_SOCKET")
                .expect("fixture Docker socket")
                .into(),
            image,
            target,
        }
    }

    fn config(&self, limits: WorkerLimits) -> DockerWorkerConfig {
        DockerWorkerConfig::new(&self.executable, &self.socket, self.image, limits).unwrap()
    }

    async fn run(&self, adapter: Adapter, name: &str, sql: &str) -> RehearsalReportV1 {
        let discovery = discover(adapter, &[], &[(name, sql.as_bytes())]);
        let plan = prepare_rehearsal(
            &discovery,
            context(self.target),
            RehearsalInput::Seeded {
                baseline_sql: BASELINE,
                seed_sql: SEED,
                seed_kind: SeedKind::Synthetic,
            },
            self.image,
        )
        .unwrap_or_else(|error| panic!("{name}: {error}, {:?}", error.coverage()));
        let report = run_rehearsal(self.config(WorkerLimits::default()), plan).await;
        report.validate().unwrap();
        assert_eq!(
            report.cleanup,
            RehearsalCleanup::Verified,
            "{name}: {:?}",
            report.failure
        );
        assert_eq!(
            report.seeded_rows_inserted,
            Some(2),
            "{name}: {:?}",
            report.failure
        );
        assert!(report.before.is_some(), "{name}: {:?}", report.failure);
        assert!(report.after.is_some(), "{name}: {:?}", report.failure);
        report
    }
}

fn added(report: &RehearsalReportV1, kind: ObjectKind, name: &str) -> bool {
    let (identity_name, identity_member) = match kind {
        ObjectKind::Column | ObjectKind::Constraint => ("items", Some(name)),
        ObjectKind::Index => (name, None),
        _ => panic!("unexpected fixture object kind: {kind:?}"),
    };
    report
        .schema_diff
        .as_ref()
        .unwrap()
        .changes
        .iter()
        .any(|change| {
            matches!(change, StructuralChange::Added { object }
            if object.identity.kind == kind
                && object.identity.schema.as_deref() == Some("public")
                && object.identity.name == identity_name
                && object.identity.member.as_deref() == identity_member
                && object.owning_table.as_ref().is_some_and(|table|
                    table.schema == "public" && table.name == "items"))
        })
}

fn expect_failure(report: &RehearsalReportV1, sqlstate: &str) {
    assert_eq!(
        report.outcome,
        RehearsalOutcome::Failed,
        "{:?}",
        report.failure
    );
    assert_eq!(report.migrations[0].state, RehearsalMigrationState::Failed);
    assert_eq!(
        report
            .failure
            .as_ref()
            .and_then(|error| error.sqlstate.as_deref()),
        Some(sqlstate)
    );
}

#[tokio::test]
#[ignore = "requires an explicit isolated Docker Engine fixture and one approved image"]
async fn isolated_rehearsal_matrix() {
    let fixture = Fixture::selected();

    let report = fixture
        .run(plain(PlainTransaction::File), "1_change.sql", DUPLICATE)
        .await;
    expect_failure(&report, "23505");
    assert_eq!(
        report.migrations[0].execution.transaction_wrapper,
        TransactionWrapper::File
    );
    assert_eq!(
        report.migrations[0].groups[0].statements[0].durability,
        Some(RehearsalDurability::RolledBack)
    );
    assert!(!added(&report, ObjectKind::Column, "added"));
    assert_eq!(
        report.migrations[0].groups[2].statements[0].state,
        RehearsalStatementState::Skipped
    );
    assert_eq!(
        report
            .failure
            .as_ref()
            .and_then(|failure| failure.span.as_ref())
            .unwrap()
            .start
            .line
            .get(),
        2
    );
    let serialized = serde_json::to_string(&report).unwrap();
    assert!(!serialized.contains("protected-seed-marker"));
    assert!(!serialized.contains("ALTER TABLE"));
    assert!(!serialized.contains("duplicate key"));

    let report = fixture
        .run(
            plain(PlainTransaction::Autocommit),
            "1_change.sql",
            DUPLICATE,
        )
        .await;
    expect_failure(&report, "23505");
    assert_eq!(
        report.migrations[0].groups[0].statements[0].durability,
        Some(RehearsalDurability::Committed)
    );
    assert!(added(&report, ObjectKind::Column, "added"));

    let report = fixture
        .run(
            plain(PlainTransaction::Autocommit),
            "1_change.sql",
            EXPLICIT,
        )
        .await;
    expect_failure(&report, "23505");
    for (index, name, durability) in [
        (1, "durable", RehearsalDurability::Committed),
        (3, "reverted", RehearsalDurability::RolledBack),
        (8, "aborted", RehearsalDurability::RolledBack),
    ] {
        let statement = &report.migrations[0].groups[index].statements[0];
        assert_eq!(statement.durability, Some(durability));
        assert!(statement.elapsed_millis.is_some());
        assert_eq!(
            added(&report, ObjectKind::Column, name),
            durability == RehearsalDurability::Committed
        );
    }

    let sql = format!("-- no-transaction\n{ALTER_AND_INDEX}");
    let report = fixture
        .run(Adapter::Sqlx(SqlxLayout::Simple), "1_change.sql", &sql)
        .await;
    expect_failure(&report, "25001");
    assert_eq!(
        report.migrations[0].groups[0].delivery,
        StatementDelivery::SingleQuery
    );
    assert_eq!(
        report.migrations[0].groups[0].statements[0].durability,
        Some(RehearsalDurability::RolledBack)
    );
    assert!(!added(&report, ObjectKind::Column, "added"));

    let sql = format!(
        "-- +goose NO TRANSACTION\n-- +goose Up\nALTER TABLE items ADD COLUMN durable integer;\n-- +goose StatementBegin\n{DUPLICATE}\n-- +goose StatementEnd\n-- +goose Down\nDROP TABLE items;\n"
    );
    let report = fixture.run(Adapter::Goose, "1_change.sql", &sql).await;
    expect_failure(&report, "23505");
    assert!(added(&report, ObjectKind::Column, "durable"));
    assert!(!added(&report, ObjectKind::Column, "added"));
    assert_eq!(report.migrations[0].groups[1].statements.len(), 3);

    let report = fixture
        .run(
            Adapter::Flyway(FlywaySettings::default()),
            "V1__change.sql",
            DUPLICATE,
        )
        .await;
    expect_failure(&report, "23505");
    assert!(!added(&report, ObjectKind::Column, "added"));

    let report = fixture
        .run(Adapter::Supabase, "20260418123456_change.sql", DUPLICATE)
        .await;
    expect_failure(&report, "23505");
    let group = &report.migrations[0].groups[0];
    assert_eq!(group.delivery, StatementDelivery::ExtendedBatch);
    assert_eq!(
        group.statements[0].durability,
        Some(RehearsalDurability::RolledBack)
    );
    assert_eq!(group.statements[2].state, RehearsalStatementState::Skipped);
    assert!(!added(&report, ObjectKind::Column, "added"));

    let sql = format!(
        "{ALTER_AND_INDEX}\nALTER TABLE items ADD COLUMN reverted integer;\nINSERT INTO items VALUES (1);"
    );
    let report = fixture
        .run(Adapter::Supabase, "20260418123456_change.sql", &sql)
        .await;
    expect_failure(&report, "23505");
    assert_eq!(
        report.migrations[0]
            .groups
            .iter()
            .map(|g| g.delivery)
            .collect::<Vec<_>>(),
        [
            StatementDelivery::ExtendedBatch,
            StatementDelivery::PerStatement,
            StatementDelivery::ExtendedBatch
        ]
    );
    assert!(added(&report, ObjectKind::Column, "added"));
    assert!(added(&report, ObjectKind::Index, "item_note_idx"));
    assert!(!added(&report, ObjectKind::Column, "reverted"));

    let report = fixture
        .run(
            plain(PlainTransaction::File),
            "1_change.sql",
            "ALTER TABLE items ADD CONSTRAINT note_present CHECK (note IS NOT NULL) NOT VALID;",
        )
        .await;
    assert_eq!(
        report.outcome,
        RehearsalOutcome::Succeeded,
        "{:?}",
        report.failure
    );
    assert!(added(&report, ObjectKind::Constraint, "note_present"));
    let after = report.after.as_ref().unwrap();
    assert!(
        after
            .constraints
            .iter()
            .any(|constraint| constraint.name == "note_present"
                && !constraint.validated
                && constraint.enforced)
    );
}

#[tokio::test]
#[ignore = "requires an explicit isolated Docker Engine fixture and one approved image"]
async fn isolated_rehearsal_distinguishes_server_and_wall_deadlines() {
    use crate::rehearsal::{plan::PlannedQuery, protocol::Session};

    let fixture = Fixture::selected();
    let limits = WorkerLimits::new(25_000, 5_000, 250, 100, 5_000).unwrap();
    DockerWorker::run(fixture.config(limits), async |worker| {
        let mut session = Session::connect(worker).await.expect("fixture session");
        let result = session
            .query(&PlannedQuery::Simple("SELECT pg_catalog.pg_sleep(1)"), 1)
            .await
            .expect("server cancellation response");
        assert_eq!(result.sqlstate.as_deref(), Some("57014"));
        Ok::<(), crate::WorkerError>(())
    })
    .await
    .unwrap();

    let discovery = discover(
        plain(PlainTransaction::Autocommit),
        &[],
        &[("1_create.sql", BASELINE)],
    );
    let plan = prepare_rehearsal(
        &discovery,
        context(fixture.target),
        RehearsalInput::EmptyDatabase,
        fixture.image,
    )
    .unwrap();
    let limits = WorkerLimits::new(25_000, 1, 500, 100, 5_000).unwrap();
    let report = run_rehearsal(fixture.config(limits), plan).await;
    report.validate().unwrap();
    assert_eq!(report.cleanup, RehearsalCleanup::Verified);
    assert_eq!(report.outcome, RehearsalOutcome::TimedOut);
    let error = report.failure.as_ref().unwrap();
    assert_eq!(error.phase, RehearsalPhase::Before);
    assert_eq!(error.reason, RehearsalFailureReason::Deadline);
    assert!(error.sqlstate.is_none());
}
