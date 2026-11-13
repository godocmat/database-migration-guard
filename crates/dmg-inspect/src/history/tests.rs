use std::{env, fs};

use dmg_core::{
    history::{NativeChecksum, NativeHistoryState, Sha384Digest},
    migration::{MigrationDirection, MigrationIdentity},
};

use super::*;

const SCHEMA: &str = "history_scope";
const QUOTED_SCHEMA: &str = "History \"quoted\"";
const QUOTED_TABLE: &str = "History.records\"2026";

fn name(schema: &str, name: &str) -> QualifiedName {
    QualifiedName {
        schema: schema.into(),
        name: name.into(),
    }
}

#[test]
fn validates_exact_history_source_and_row_bounds() {
    let good = name(SCHEMA, "_sqlx_migrations");
    assert!(matches!(
        HistoryOptions::new(14, MigrationAdapter::Sqlx, good.clone()),
        Err(HistoryError::UnsupportedMajor)
    ));
    assert!(matches!(
        HistoryOptions::new(18, MigrationAdapter::Plain, good.clone()),
        Err(HistoryError::UnsupportedAdapter)
    ));
    for source in [
        name("", "table"),
        name("pg_catalog", "pg_class"),
        name("information_schema", "tables"),
        name(SCHEMA, "bad\nname"),
        name(SCHEMA, &"x".repeat(64)),
        name(SCHEMA, ""),
    ] {
        assert!(matches!(
            HistoryOptions::new(18, MigrationAdapter::Sqlx, source),
            Err(HistoryError::InvalidSource)
        ));
    }
    let source = name(QUOTED_SCHEMA, QUOTED_TABLE);
    assert_eq!(
        reader::qualified_name(&source),
        "\"History \"\"quoted\"\"\".\"History.records\"\"2026\""
    );
    let options = HistoryOptions::new(18, MigrationAdapter::Sqlx, source).unwrap();
    assert_eq!(options.row_limit, DEFAULT_ROW_LIMIT);
    assert!(matches!(
        options.clone().with_row_limit(0),
        Err(HistoryError::InvalidRowLimit)
    ));
    assert!(matches!(
        options.clone().with_row_limit(MAX_ROW_LIMIT + 1),
        Err(HistoryError::InvalidRowLimit)
    ));
    assert!(options.with_row_limit(MAX_ROW_LIMIT).is_ok());
}

#[tokio::test]
#[ignore = "requires the disposable PostgreSQL native history fixture"]
async fn native_history() {
    let major: u32 = env::var("DMG_HISTORY_TEST_MAJOR").unwrap().parse().unwrap();
    let scenario = env::var("DMG_HISTORY_TEST_CASE").unwrap();
    let password = env::var("DMG_TEST_POSTGRES_PASSWORD").unwrap();
    let ca = fs::read(env::var_os("DMG_TEST_POSTGRES_CA").unwrap()).unwrap();
    let port = env::var("DMG_TEST_POSTGRES_PORT").unwrap().parse().unwrap();
    let target = InspectionTarget::new(
        "localhost",
        port,
        "postgres",
        "dmg_catalog_reader",
        password.as_bytes().to_vec(),
        &ca,
    )
    .unwrap();
    let options = HistoryOptions::new(
        major,
        MigrationAdapter::Sqlx,
        name(QUOTED_SCHEMA, QUOTED_TABLE),
    )
    .unwrap();
    if scenario == "contention" {
        for (timeouts, reason) in [
            (
                InspectionTimeouts::new(3000, 2000, 40, 2000).unwrap(),
                Reason::Contention,
            ),
            (
                InspectionTimeouts::new(3000, 40, 2000, 2000).unwrap(),
                Reason::Cancelled,
            ),
            (
                InspectionTimeouts::new(600, 4000, 4000, 2000).unwrap(),
                Reason::Deadline,
            ),
        ] {
            let evidence = inspect(&target, &options, timeouts).await;
            assert_eq!(
                evidence.observation().server_version_num.unwrap() / 10_000,
                major
            );
            assert_unavailable(&evidence, Field::History, reason);
        }
        return;
    }
    if scenario == "replacement" {
        let timeouts = InspectionTimeouts::new(4000, 3000, 2500, 2000).unwrap();
        let evidence = inspect(&target, &options, timeouts).await;
        assert_unavailable(&evidence, Field::History, Reason::InvalidValue);
        return;
    }
    assert_eq!(scenario, "ordinary");

    let timeouts = InspectionTimeouts::default();
    let sqlx = inspect(&target, &options, timeouts).await;
    assert_eq!(
        sqlx.observation().server_version_num.unwrap() / 10_000,
        major
    );
    assert!(sqlx.observation().observed_at_unix_seconds > 0);
    assert_eq!(sqlx.coverage(), NativeHistoryCoverage::Complete);
    assert_eq!(sqlx.source(), Some(&options.source));
    assert_eq!(sqlx.rows().len(), 3);
    let [applied, dirty, bad_checksum] = sqlx.rows() else {
        panic!("expected three normalized SQLx events");
    };
    assert_eq!(applied.identity.version.as_numeric(), Some(-3));
    assert_eq!(applied.state, NativeHistoryState::Applied);
    assert_eq!(
        applied.checksum,
        Some(NativeChecksum::SqlxSha384 {
            digest: Sha384Digest::from_bytes([0x4c; 48]),
        })
    );
    assert_eq!(dirty.identity.version.as_numeric(), Some(2));
    assert_eq!(dirty.state, NativeHistoryState::Failed);
    assert_eq!(bad_checksum.identity.version.as_numeric(), Some(5));
    assert!(bad_checksum.checksum.is_none());
    assert_eq!(
        sqlx.gaps(),
        &[InspectionGap {
            scope: InspectionGapScope::Migration {
                identity: bad_checksum.identity.clone(),
            },
            field: Field::NativeChecksum,
            reason: Reason::InvalidValue,
        }]
    );
    let limited = inspect(
        &target,
        &options.clone().with_row_limit(2).unwrap(),
        timeouts,
    )
    .await;
    assert_eq!(limited.coverage(), NativeHistoryCoverage::Incomplete);
    assert_eq!(limited.rows(), &sqlx.rows()[..2]);
    assert_eq!(
        limited.gaps(),
        &[target_gap(Field::History, Reason::Truncated)]
    );
    let mut mismatched = options.clone();
    mismatched.expected_major = if major == 18 { 17 } else { 18 };
    let evidence = inspect(&target, &mismatched, timeouts).await;
    assert_unavailable(&evidence, Field::ServerVersion, Reason::Unsupported);

    for (schema, table, reason) in [
        (SCHEMA, "missing", Reason::Missing),
        ("missing_schema", "history", Reason::Missing),
        ("catalog_hidden", "history", Reason::PermissionDenied),
        (SCHEMA, "private", Reason::PermissionDenied),
        (SCHEMA, "row_security", Reason::PermissionDenied),
        (SCHEMA, "wrong_shape", Reason::InvalidValue),
        (SCHEMA, "without_primary_key", Reason::InvalidValue),
        (SCHEMA, "generated_column", Reason::InvalidValue),
        (SCHEMA, "view", Reason::Unsupported),
        (SCHEMA, "foreign_table", Reason::Unsupported),
        (SCHEMA, "partition_parent", Reason::Unsupported),
        (SCHEMA, "partition_leaf", Reason::Unsupported),
    ] {
        let options =
            HistoryOptions::new(major, MigrationAdapter::Sqlx, name(schema, table)).unwrap();
        let evidence = inspect(&target, &options, timeouts).await;
        assert_unavailable(&evidence, Field::History, reason);
    }
    let empty = HistoryOptions::new(major, MigrationAdapter::Sqlx, name(SCHEMA, "empty")).unwrap();
    let evidence = inspect(&target, &empty, timeouts).await;
    assert_eq!(evidence.coverage(), NativeHistoryCoverage::Complete);
    assert!(evidence.gaps().is_empty());
    assert!(evidence.rows().is_empty());

    let flyway = HistoryOptions::new(
        major,
        MigrationAdapter::Flyway,
        name(SCHEMA, "flyway_schema_history"),
    )
    .unwrap();
    let evidence = inspect(&target, &flyway, timeouts).await;
    assert_eq!(evidence.coverage(), NativeHistoryCoverage::Complete);
    assert!(evidence.gaps().is_empty());
    let rows = evidence.rows();
    assert_eq!(rows.len(), 5);
    assert_eq!(
        rows[0].identity,
        MigrationIdentity::flyway_version("1.2").unwrap()
    );
    assert_eq!(
        rows[0].checksum,
        Some(NativeChecksum::FlywayCrc32 { value: -123 })
    );
    assert_eq!(rows[1].direction, MigrationDirection::Down);
    assert_eq!(rows[1].state, NativeHistoryState::Applied);
    assert_eq!(
        rows[1].checksum,
        Some(NativeChecksum::FlywayCrc32 { value: 22 })
    );
    assert_eq!(rows[2].state, NativeHistoryState::Failed);
    assert_eq!(
        rows[3].identity,
        MigrationIdentity::flyway_repeatable("refresh summary").unwrap()
    );
    assert_eq!(rows[4].identity, rows[3].identity);
    assert_eq!(rows[4].rank, Some(5));
    assert_eq!(
        rows[4].checksum,
        Some(NativeChecksum::FlywayCrc32 { value: -9 })
    );

    let mut unsupported = flyway.clone();
    unsupported.source.name = "unsupported_flyway".into();
    let evidence = inspect(&target, &unsupported, timeouts).await;
    assert_eq!(evidence.coverage(), NativeHistoryCoverage::Incomplete);
    assert_eq!(evidence.rows().len(), 1);
    assert_eq!(
        evidence.gaps(),
        &[target_gap(Field::History, Reason::Unsupported)]
    );

    let goose = HistoryOptions::new(
        major,
        MigrationAdapter::Goose,
        name(SCHEMA, "goose_db_version"),
    )
    .unwrap();
    let evidence = inspect(&target, &goose, timeouts).await;
    assert_eq!(evidence.coverage(), NativeHistoryCoverage::Complete);
    assert_eq!(evidence.rows().len(), 3);
    assert_eq!(evidence.rows()[0].rank, Some(2));
    assert_eq!(evidence.rows()[1].state, NativeHistoryState::Unknown);
    assert_eq!(evidence.rows()[2].state, NativeHistoryState::Applied);
    assert!(evidence.rows().iter().all(|row| row.checksum.is_none()));
    let mut goose_empty = goose.clone();
    goose_empty.source.name = "goose_empty".into();
    let evidence = inspect(&target, &goose_empty, timeouts).await;
    assert_eq!(evidence.coverage(), NativeHistoryCoverage::Complete);
    assert!(evidence.rows().is_empty());

    let supabase = HistoryOptions::new(
        major,
        MigrationAdapter::Supabase,
        name("supabase_migrations", "schema_migrations"),
    )
    .unwrap();
    let evidence = inspect(&target, &supabase, timeouts).await;
    assert_eq!(evidence.coverage(), NativeHistoryCoverage::Complete);
    assert_eq!(
        evidence
            .rows()
            .iter()
            .map(|row| row.identity.version.to_string())
            .collect::<Vec<_>>(),
        ["01", "1", "10", "2"]
    );
    let mut oversized = supabase.clone();
    oversized.source.name = "oversized".into();
    let evidence = inspect(&target, &oversized, timeouts).await;
    assert_eq!(evidence.coverage(), NativeHistoryCoverage::Incomplete);
    assert_eq!(evidence.rows().len(), 1);
    assert_eq!(
        evidence.gaps(),
        &[
            target_gap(Field::History, Reason::Truncated),
            target_gap(Field::History, Reason::InvalidValue),
        ],
    );

    let invalid = InspectionTarget::new(
        "localhost",
        port,
        "postgres",
        "dmg_catalog_reader",
        b"fixture-wrong-password".to_vec(),
        &ca,
    )
    .unwrap();
    let evidence = inspect(&invalid, &options, timeouts).await;
    assert_unavailable(&evidence, Field::Connection, Reason::PermissionDenied);
    assert!(evidence.observation().server_version_num.is_none());
}

async fn inspect(
    target: &InspectionTarget,
    options: &HistoryOptions,
    timeouts: InspectionTimeouts,
) -> NativeHistoryEvidenceV1 {
    let target_id = TargetId::new("history-fixture").unwrap();
    let inventory = ContentDigest::of(b"fixture forward inventory");
    let result = target
        .inspect_history(target_id.clone(), inventory, options, timeouts)
        .await
        .unwrap();
    assert_eq!(result.observation().target_id, target_id);
    assert_eq!(result.inventory_digest(), inventory);
    assert_eq!(result.adapter(), options.adapter);
    result
}

fn assert_unavailable(evidence: &NativeHistoryEvidenceV1, field: Field, reason: Reason) {
    assert_eq!(
        evidence.coverage(),
        NativeHistoryCoverage::Unavailable,
        "{evidence:?}"
    );
    assert!(evidence.rows().is_empty());
    assert_eq!(
        evidence.gaps(),
        &[target_gap(field, reason)],
        "{evidence:?}"
    );
}
