use std::{env, time::Duration};

use dmg_core::rehearsal::{
    RehearsalDurability, RehearsalFailureReason, RehearsalPhase, RehearsalStatementState,
};
use dmg_discovery::{FlywaySettings, SqlxLayout};
use tokio::{net::TcpStream, time::timeout};
use tokio_postgres::{Config, NoTls};

use super::*;
use crate::rehearsal::{
    plan::PlannedQuery,
    protocol::{Ready, Session},
    runner::{execute_migration, initialize_session, run_script},
};

const BASELINE: &[u8] = b"CREATE TABLE items (id integer PRIMARY KEY, note text);";
const SEED: &[u8] = b"INSERT INTO items VALUES (1, 'fixture-note'), (2, NULL);";
const LATE_FAILURE: &str =
    "ALTER TABLE items ADD COLUMN added integer;\nINSERT INTO items VALUES (1);\nSELECT 9;";
const CONCURRENT: &str = "ALTER TABLE items ADD COLUMN added integer;\nCREATE INDEX CONCURRENTLY item_note_idx ON items (note);";
const EXPLICIT: &str = "BEGIN;\nALTER TABLE items ADD COLUMN durable integer;\nSAVEPOINT before_add;\nALTER TABLE items ADD COLUMN reverted integer;\nROLLBACK TO before_add;\nRELEASE before_add;\nCOMMIT;\nBEGIN;\nALTER TABLE items ADD COLUMN aborted integer;\nINSERT INTO items VALUES (1);";
const SCHEMA: &str = "dmg_rehearsal_fixture";

struct Case<'a> {
    adapter: Adapter,
    name: &'static str,
    sql: &'a str,
    error: Option<&'static str>,
    added: &'a [&'static str],
}

#[tokio::test]
#[ignore = "requires an explicitly created disposable PostgreSQL protocol fixture"]
async fn isolated_postgres_protocol() {
    timeout(Duration::from_secs(30), protocol_matrix())
        .await
        .expect("protocol matrix deadline");
}

async fn protocol_matrix() {
    let port = env::var("DMG_PROTOCOL_TEST_PORT")
        .expect("fixture port")
        .parse::<u16>()
        .unwrap();
    let password = env::var("DMG_PROTOCOL_TEST_PASSWORD").expect("fixture SCRAM password");
    let (image, target) = match env::var("DMG_PROTOCOL_TEST_POSTGRES").as_deref() {
        Ok("15.20") => (PostgresImage::V15_20, PostgresTarget::V15),
        Ok("16.16") => (PostgresImage::V16_16, PostgresTarget::V16),
        Ok("17.12") => (PostgresImage::V17_12, PostgresTarget::V17),
        Ok("18.7") | Err(env::VarError::NotPresent) => (PostgresImage::V18_7, PostgresTarget::V18),
        _ => panic!("fixture PostgreSQL release"),
    };
    let mut config = Config::new();
    config
        .host("127.0.0.1")
        .port(port)
        .user("dmg_rehearsal_fixture")
        .dbname("dmg_rehearsal_fixture")
        .password(password)
        .options("-c statement_timeout=1000ms");
    let (client, connection) = config.connect(NoTls).await.unwrap();
    let driver = tokio::spawn(connection);
    let row = client
        .query_one(
            "SELECT current_setting('server_version_num')::integer,
            current_database() = 'dmg_rehearsal_fixture',
            current_user = 'dmg_rehearsal_fixture',
            pg_catalog.inet_server_addr() = '127.0.0.1'::inet",
            &[],
        )
        .await
        .unwrap();
    assert_eq!(
        row.get::<_, i32>(0),
        i32::try_from(image.server_version_num()).unwrap()
    );
    assert!((1..4).all(|index| row.get::<_, bool>(index)));

    let sqlx_opt_out = format!("-- no-transaction\n{CONCURRENT}");
    let goose = format!(
        "-- +goose NO TRANSACTION\n-- +goose Up\nALTER TABLE items ADD COLUMN durable integer;\n-- +goose StatementBegin\n{LATE_FAILURE}\n-- +goose StatementEnd\n-- +goose Down\nDROP TABLE items;\n"
    );
    let supabase_split = format!(
        "{CONCURRENT}\nALTER TABLE items ADD COLUMN reverted integer;\nINSERT INTO items VALUES (1);"
    );
    let cases = [
        Case {
            adapter: plain(PlainTransaction::File),
            name: "1_change.sql",
            sql: LATE_FAILURE,
            error: Some("23505"),
            added: &[],
        },
        Case {
            adapter: plain(PlainTransaction::Autocommit),
            name: "1_change.sql",
            sql: LATE_FAILURE,
            error: Some("23505"),
            added: &["added"],
        },
        Case {
            adapter: plain(PlainTransaction::Autocommit),
            name: "1_change.sql",
            sql: EXPLICIT,
            error: Some("23505"),
            added: &["durable"],
        },
        Case {
            adapter: Adapter::Sqlx(SqlxLayout::Simple),
            name: "1_change.sql",
            sql: &sqlx_opt_out,
            error: Some("25001"),
            added: &[],
        },
        Case {
            adapter: Adapter::Goose,
            name: "1_change.sql",
            sql: &goose,
            error: Some("23505"),
            added: &["durable"],
        },
        Case {
            adapter: Adapter::Flyway(FlywaySettings::default()),
            name: "V1__change.sql",
            sql: LATE_FAILURE,
            error: Some("23505"),
            added: &[],
        },
        Case {
            adapter: Adapter::Supabase,
            name: "20260418123456_change.sql",
            sql: LATE_FAILURE,
            error: Some("23505"),
            added: &[],
        },
        Case {
            adapter: Adapter::Supabase,
            name: "20260418123456_change.sql",
            sql: &supabase_split,
            error: Some("23505"),
            added: &["added"],
        },
        Case {
            adapter: plain(PlainTransaction::File),
            name: "1_change.sql",
            sql: "ALTER TABLE items ADD CONSTRAINT note_present CHECK (note IS NOT NULL) NOT VALID;",
            error: None,
            added: &[],
        },
    ];

    for case in cases {
        client.batch_execute("DROP SCHEMA IF EXISTS dmg_rehearsal_fixture CASCADE; CREATE SCHEMA dmg_rehearsal_fixture;").await.unwrap();
        let discovery = discover(case.adapter, &[], &[(case.name, case.sql.as_bytes())]);
        let mut context = context(target);
        context.scope = InspectionScope::new(vec![SCHEMA.into()]).unwrap();
        context.search_path = vec![SCHEMA.into()];
        let mut plan = prepare_rehearsal(
            &discovery,
            context,
            RehearsalInput::Seeded {
                baseline_sql: BASELINE,
                seed_sql: SEED,
                seed_kind: SeedKind::Synthetic,
            },
            image,
        )
        .unwrap_or_else(|error| panic!("{}: {error}, {:?}", case.name, error.coverage()));

        let stream = TcpStream::connect(("127.0.0.1", port)).await.unwrap();
        let mut session = Session::startup(stream, &config).await.unwrap();
        initialize_session(
            &mut session,
            &plan.binding().search_path,
            RehearsalPhase::Baseline,
        )
        .await
        .unwrap();
        run_script(
            &mut session,
            plan.baseline.unwrap(),
            RehearsalPhase::Baseline,
        )
        .await
        .unwrap();
        assert_eq!(
            run_script(&mut session, plan.seed.unwrap(), RehearsalPhase::Seed)
                .await
                .unwrap(),
            2
        );
        let planned = plan.migrations.pop().unwrap();
        let mut migration = planned.observation.clone();
        let result = execute_migration(&mut session, planned, &mut migration, &mut None).await;
        match case.error {
            Some(expected) => {
                let error = result.unwrap_err();
                assert_eq!(error.reason, RehearsalFailureReason::Database);
                assert_eq!(error.sqlstate.as_deref(), Some(expected), "{}", case.name);
                assert!(error.span.is_some(), "{}", case.name);
                assert!(
                    migration
                        .groups
                        .iter()
                        .flat_map(|group| &group.statements)
                        .any(|statement| statement.state == RehearsalStatementState::Failed)
                );
            }
            None => result.unwrap(),
        }

        let columns = client
            .query(
                "SELECT column_name::text FROM information_schema.columns
            WHERE table_schema = 'dmg_rehearsal_fixture' AND table_name = 'items'
            AND ordinal_position > 2 ORDER BY ordinal_position",
                &[],
            )
            .await
            .unwrap();
        let names = columns
            .iter()
            .map(|row| row.get::<_, &str>(0))
            .collect::<Vec<_>>();
        assert_eq!(names, case.added, "{}", case.name);
        if case.sql == EXPLICIT {
            for (index, expected) in [
                (1, RehearsalDurability::Committed),
                (3, RehearsalDurability::RolledBack),
                (8, RehearsalDurability::RolledBack),
            ] {
                assert_eq!(
                    migration.groups[index].statements[0].durability,
                    Some(expected)
                );
            }
        }
        if case.sql == supabase_split {
            let exists = client
                .query_one(
                    "SELECT pg_catalog.to_regclass(
                'dmg_rehearsal_fixture.item_note_idx') IS NOT NULL",
                    &[],
                )
                .await
                .unwrap();
            assert!(exists.get::<_, bool>(0));
            assert_eq!(
                migration.groups[0].delivery,
                StatementDelivery::ExtendedBatch
            );
            assert_eq!(
                migration.groups[0].statements[0].durability,
                Some(RehearsalDurability::Committed)
            );
            assert_eq!(
                migration.groups[2].statements[0].durability,
                Some(RehearsalDurability::RolledBack)
            );
        }
        if case.error.is_none() {
            let row = client.query_one("SELECT convalidated FROM pg_catalog.pg_constraint
                WHERE conname = 'note_present' AND connamespace = 'dmg_rehearsal_fixture'::regnamespace", &[]).await.unwrap();
            assert!(!row.get::<_, bool>(0));
        }

        let result = session
            .query(&PlannedQuery::Simple("SELECT 1"), 1)
            .await
            .unwrap();
        assert_eq!(result.ready, Ready::Idle);
    }

    let stream = TcpStream::connect(("127.0.0.1", port)).await.unwrap();
    let mut session = Session::startup(stream, &config).await.unwrap();
    let result = session
        .query(&PlannedQuery::Simple("SELECT pg_catalog.pg_sleep(2)"), 1)
        .await
        .unwrap();
    assert_eq!(result.sqlstate.as_deref(), Some("57014"));
    driver.abort();
}
