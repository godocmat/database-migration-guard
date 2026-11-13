use dmg_core::{
    coverage::DiagnosticScope,
    history::{
        NativeChecksum, NativeHistoryCoverage, NativeHistoryEvidenceV1, NativeHistoryMethod,
        NativeHistoryRow, NativeHistoryState, forward_inventory_digest,
    },
    inspection::{
        InspectionField, InspectionGap, InspectionGapReason, InspectionGapScope, Observation,
        TargetId,
    },
    migration::{
        AppliedHistoryBasis, AppliedStatus, MigrationAdapter, MigrationDirection, MigrationIdentity,
    },
    snapshot::QualifiedName,
};
use dmg_discovery::{
    Discovery, FlywaySettings, NativeHistoryError, NativeHistoryReconciliation, SqlxLayout,
    reconcile_native_history,
};

use super::*;

fn discover(repo: &TestRepo, adapter: Adapter) -> Discovery {
    let options = DiscoveryOptions::new(
        vec![RepoPath::new("migrations").unwrap()],
        adapter,
        DiscoveryLimits::default(),
    )
    .unwrap();
    discover_worktree(&repo.0, &options)
}

fn target() -> TargetId {
    TargetId::new("authorized-target").unwrap()
}

fn evidence(
    discovery: &Discovery,
    adapter: MigrationAdapter,
    coverage: NativeHistoryCoverage,
    rows: Vec<NativeHistoryRow>,
    gaps: Vec<InspectionGap>,
) -> NativeHistoryEvidenceV1 {
    let source = match adapter {
        MigrationAdapter::Plain => None,
        MigrationAdapter::Sqlx => Some(("public", "_sqlx_migrations")),
        MigrationAdapter::Flyway => Some(("public", "flyway_schema_history")),
        MigrationAdapter::Goose => Some(("public", "goose_db_version")),
        MigrationAdapter::Supabase => Some(("supabase_migrations", "schema_migrations")),
    };
    NativeHistoryEvidenceV1::new(
        Observation {
            target_id: target(),
            server_version_num: Some(170012),
            observed_at_unix_seconds: 1_700_000_000,
        },
        adapter,
        source.map(|(schema, name)| QualifiedName {
            schema: schema.into(),
            name: name.into(),
        }),
        forward_inventory_digest(adapter, &discovery.plan.inventory).unwrap(),
        coverage,
        rows,
        gaps,
    )
    .unwrap()
}

fn sqlx_row(version: i64, state: NativeHistoryState, sha384: Option<&str>) -> NativeHistoryRow {
    NativeHistoryRow {
        identity: MigrationIdentity::numeric(MigrationAdapter::Sqlx, version),
        direction: MigrationDirection::Forward,
        rank: None,
        state,
        checksum: sha384.map(|hex| NativeChecksum::SqlxSha384 {
            digest: hex.parse().unwrap(),
        }),
    }
}

fn flyway_row(
    identity: &MigrationIdentity,
    direction: MigrationDirection,
    rank: u64,
    state: NativeHistoryState,
    crc32: Option<i32>,
) -> NativeHistoryRow {
    NativeHistoryRow {
        identity: identity.clone(),
        direction,
        rank: Some(rank),
        state,
        checksum: crc32.map(|value| NativeChecksum::FlywayCrc32 { value }),
    }
}

fn statuses(result: &NativeHistoryReconciliation) -> Vec<AppliedStatus> {
    result
        .applied_history
        .migrations
        .iter()
        .map(|migration| migration.status)
        .collect()
}

#[test]
fn sqlx_classifies_observed_versions_using_native_original_byte_checksums() {
    use AppliedStatus::{Applied, Failed, Missing, Modified, Pending, Unknown};
    use NativeHistoryState::{Applied as Recorded, Failed as Dirty};

    let repo = TestRepo::new().unwrap();
    // S01 recorded different native checksums for these two versions of the same migration.
    let lf = include_bytes!("../../../../fixtures/dmg-02/inputs/spans-lf.sql");
    let crlf = include_bytes!("../../../../fixtures/dmg-02/inputs/spans-crlf.sql");
    let lf_sha384 = "194d586fb33868dbd576cc8c2e38dd361db29c743c05a68cb96a6dde07ac8f80dcf1e507b7e7d11a00e0728058a46589";
    let crlf_sha384 = "e1a30752ec7af1ab03a3584b35dd5ccbe37b0c4e78c8f7314b2c30d912d3edc27d3935282b3329a19cdc7339b8f01623";
    repo.write("migrations/1_lf.sql", lf);
    repo.write("migrations/2_crlf.sql", crlf);
    repo.write("migrations/3_crlf.sql", crlf);
    for (version, name) in [(4, "pending"), (5, "dirty"), (6, "unknown")] {
        repo.write(format!("migrations/{version}_{name}.sql"), b"SELECT 1;");
    }
    let discovery = discover(&repo, Adapter::Sqlx(SqlxLayout::Simple));
    let plan_before = discovery.plan.clone();
    let history = evidence(
        &discovery,
        MigrationAdapter::Sqlx,
        NativeHistoryCoverage::Complete,
        vec![
            sqlx_row(1, Recorded, Some(lf_sha384)),
            sqlx_row(2, Recorded, Some(crlf_sha384)),
            sqlx_row(3, Recorded, Some(lf_sha384)),
            sqlx_row(5, Dirty, None),
            sqlx_row(6, Recorded, None),
            sqlx_row(7, Recorded, Some(lf_sha384)),
        ],
        vec![],
    );

    let result = reconcile_native_history(&discovery, &target(), &history).unwrap();
    assert_eq!(
        statuses(&result),
        [
            Applied, Applied, Modified, Pending, Failed, Unknown, Missing
        ]
    );
    assert_eq!(result.coverage.gaps().len(), 1);
    assert_eq!(
        result.applied_history.basis,
        AppliedHistoryBasis::Native {
            digest: history.digest().unwrap(),
            inventory_digest: history.inventory_digest(),
            adapter: MigrationAdapter::Sqlx,
            method: NativeHistoryMethod::RunnerChecksum,
        }
    );
    assert_eq!(discovery.plan, plan_before);
}

#[test]
fn native_history_binds_adapter_target_paths_and_original_bytes() {
    let repo = TestRepo::new().unwrap();
    repo.write("migrations/1_a.sql", b"SELECT 1;\n");
    let adapter = Adapter::Sqlx(SqlxLayout::Simple);
    let discovery = discover(&repo, adapter.clone());
    let history = evidence(
        &discovery,
        MigrationAdapter::Sqlx,
        NativeHistoryCoverage::Complete,
        vec![],
        vec![],
    );
    assert!(matches!(
        reconcile_native_history(
            &discovery,
            &TargetId::new("different-target").unwrap(),
            &history
        ),
        Err(NativeHistoryError::WrongTarget)
    ));
    let goose = discover(&repo, Adapter::Goose);
    assert!(matches!(
        reconcile_native_history(&goose, &target(), &history),
        Err(NativeHistoryError::WrongAdapter)
    ));
    let plain = discover_worktree(&repo.0, &plain(DiscoveryLimits::default()));
    assert!(matches!(
        reconcile_native_history(&plain, &target(), &history),
        Err(NativeHistoryError::UnsupportedAdapter)
    ));
    repo.write("migrations/1_a.sql", b"SELECT 1;\r\n");
    let changed = discover(&repo, adapter.clone());
    assert!(matches!(
        reconcile_native_history(&changed, &target(), &history),
        Err(NativeHistoryError::WrongInventory)
    ));
    fs::rename(
        repo.0.join("migrations/1_a.sql"),
        repo.0.join("migrations/1_b.sql"),
    )
    .unwrap();
    repo.write("migrations/1_b.sql", b"SELECT 1;\n");
    let moved = discover(&repo, adapter);
    assert!(matches!(
        reconcile_native_history(&moved, &target(), &history),
        Err(NativeHistoryError::WrongInventory)
    ));
}

#[test]
fn incomplete_reads_preserve_a_verified_sqlx_failure_and_unknown_absences() {
    let repo = TestRepo::new().unwrap();
    repo.write("migrations/1_a.sql", b"SELECT 1;");
    repo.write("migrations/2_b.sql", b"SELECT 2;");
    let discovery = discover(&repo, Adapter::Sqlx(SqlxLayout::Simple));
    for (coverage, reason, rows, expected) in [
        (
            NativeHistoryCoverage::Incomplete,
            InspectionGapReason::Truncated,
            vec![sqlx_row(2, NativeHistoryState::Failed, None)],
            [AppliedStatus::Unknown, AppliedStatus::Failed],
        ),
        (
            NativeHistoryCoverage::Unavailable,
            InspectionGapReason::PermissionDenied,
            vec![],
            [AppliedStatus::Unknown, AppliedStatus::Unknown],
        ),
    ] {
        let history = evidence(
            &discovery,
            MigrationAdapter::Sqlx,
            coverage,
            rows,
            vec![InspectionGap {
                scope: InspectionGapScope::Target,
                field: InspectionField::History,
                reason,
            }],
        );
        let result = reconcile_native_history(&discovery, &target(), &history).unwrap();
        assert_eq!(statuses(&result), expected);
        assert_eq!(result.coverage.gaps().len(), 1);
        assert_eq!(
            result.coverage.gaps()[0].kind,
            CoverageGapKind::MissingAppliedHistory
        );
    }
}

#[test]
fn checksum_gap_preserves_complete_row_coverage() {
    let repo = TestRepo::new().unwrap();
    let source = b"SELECT 1;\n";
    repo.write("migrations/1_a.sql", source);
    repo.write("migrations/2_b.sql", b"SELECT 2;");
    let discovery = discover(&repo, Adapter::Sqlx(SqlxLayout::Simple));
    let mut row = sqlx_row(1, NativeHistoryState::Applied, None);
    row.checksum = Some(NativeChecksum::SqlxSha384 {
        digest: dmg_core::history::Sha384Digest::of(source),
    });
    let scope = InspectionGapScope::Migration {
        identity: row.identity.clone(),
    };
    let history = evidence(
        &discovery,
        MigrationAdapter::Sqlx,
        NativeHistoryCoverage::Complete,
        vec![row],
        vec![InspectionGap {
            scope,
            field: InspectionField::NativeChecksum,
            reason: InspectionGapReason::InvalidValue,
        }],
    );
    let result = reconcile_native_history(&discovery, &target(), &history).unwrap();
    assert_eq!(
        statuses(&result),
        [AppliedStatus::Unknown, AppliedStatus::Pending]
    );
    assert!(matches!(
        result.coverage.gaps()[0].scope,
        DiagnosticScope::Migration { .. }
    ));
}

#[test]
fn incomplete_head_cannot_turn_a_history_only_row_into_missing() {
    let repo = TestRepo::new().unwrap();
    repo.write("migrations/1_a.sql", b"SELECT 1;");
    let options = DiscoveryOptions::new(
        vec![RepoPath::new("migrations").unwrap()],
        Adapter::Sqlx(SqlxLayout::Simple),
        DiscoveryLimits {
            max_file_bytes: 1,
            ..DiscoveryLimits::default()
        },
    )
    .unwrap();
    let discovery = discover_worktree(&repo.0, &options);
    assert!(discovery.plan.inventory.is_empty());
    let history = evidence(
        &discovery,
        MigrationAdapter::Sqlx,
        NativeHistoryCoverage::Complete,
        vec![sqlx_row(1, NativeHistoryState::Applied, None)],
        vec![],
    );

    let result = reconcile_native_history(&discovery, &target(), &history).unwrap();
    assert_eq!(statuses(&result), [AppliedStatus::Unknown]);
    assert_eq!(result.coverage.status(), CoverageStatus::Incomplete);
}

#[test]
fn flyway_uses_signed_crc32_and_the_latest_repeatable_or_version_event() {
    use AppliedStatus::{Applied, Failed, Pending, Unknown};
    use MigrationDirection::{Down, Forward};
    use NativeHistoryState::{Applied as Recorded, Failed as Rejected, Unknown as Unrecognized};

    let repo = TestRepo::new().unwrap();
    // CRLF and a BOM preserve the signed checksum of the pinned Flyway ordering fixture.
    repo.write(
        "migrations/V1__base.sql",
        b"\xef\xbb\xbfCREATE TABLE ordered (rank integer GENERATED ALWAYS AS IDENTITY, marker text);\r\nINSERT INTO ordered (marker) VALUES ('base');\r\n",
    );
    for version in 2..=5 {
        repo.write(format!("migrations/V{version}__example.sql"), b"SELECT 1;");
    }
    // The native runner stored this checksum with a BOM, CRLF, and non-ASCII SQL.
    repo.write(
        "migrations/V6__bom.sql",
        "CREATE TABLE marker (label text);\nINSERT INTO marker VALUES ('\u{e9}\u{1f9ea}');\n"
            .as_bytes(),
    );
    repo.write("migrations/U2__example.sql", b"SELECT 2;");
    repo.write(
        "migrations/R__alpha_z.sql",
        b"INSERT INTO ordered (marker) VALUES ('alpha z revised');\n",
    );
    let discovery = discover(&repo, Adapter::Flyway(FlywaySettings::default()));
    let version = |version| MigrationIdentity::flyway_version(version).unwrap();
    let repeatable = MigrationIdentity::flyway_repeatable("alpha z").unwrap();
    let history = evidence(
        &discovery,
        MigrationAdapter::Flyway,
        NativeHistoryCoverage::Complete,
        vec![
            flyway_row(&repeatable, Forward, 9, Recorded, Some(2_001_573_982)),
            flyway_row(&version("01.0"), Forward, 1, Recorded, Some(-1_859_455_881)),
            flyway_row(&version("2"), Forward, 2, Recorded, None),
            flyway_row(&version("2"), Down, 3, Recorded, None),
            flyway_row(&version("3"), Forward, 4, Recorded, None),
            flyway_row(&version("3"), Down, 5, Rejected, None),
            flyway_row(&version("4"), Forward, 6, Recorded, None),
            flyway_row(&version("4"), Forward, 7, Unrecognized, None),
            flyway_row(&version("6"), Forward, 10, Recorded, Some(241_984_176)),
            flyway_row(&repeatable, Forward, 8, Recorded, Some(-246_280_505)),
        ],
        vec![],
    );
    let result = reconcile_native_history(&discovery, &target(), &history).unwrap();
    assert_eq!(
        statuses(&result),
        [Applied, Pending, Failed, Unknown, Pending, Applied, Applied]
    );
    assert_eq!(result.coverage.gaps().len(), 1);

    let incomplete = evidence(
        &discovery,
        MigrationAdapter::Flyway,
        NativeHistoryCoverage::Incomplete,
        history.rows().to_vec(),
        vec![],
    );
    let incomplete = reconcile_native_history(&discovery, &target(), &incomplete).unwrap();
    assert_eq!(statuses(&incomplete), [Unknown; 7]);
}

#[test]
fn flyway_leaves_placeholder_checksum_unknown() {
    let repo = TestRepo::new().unwrap();
    let sql = "SELECT '${flyway:timestamp}';";
    repo.write("migrations/R__refresh.sql", sql.as_bytes());
    let discovery = discover(&repo, Adapter::Flyway(FlywaySettings::default()));
    let repeatable = MigrationIdentity::flyway_repeatable("refresh").unwrap();
    let history = evidence(
        &discovery,
        MigrationAdapter::Flyway,
        NativeHistoryCoverage::Complete,
        vec![flyway_row(
            &repeatable,
            MigrationDirection::Forward,
            1,
            NativeHistoryState::Applied,
            Some(42),
        )],
        vec![],
    );
    let result = reconcile_native_history(&discovery, &target(), &history).unwrap();
    assert_eq!(statuses(&result), [AppliedStatus::Unknown]);
    assert_eq!(result.coverage.status(), CoverageStatus::Incomplete);
}

#[test]
fn goose_and_supabase_classify_presence_without_claiming_content_integrity() {
    use AppliedStatus::{Applied, Missing, Pending};

    for (adapter, kind, applied, missing) in [
        (
            Adapter::Goose,
            MigrationAdapter::Goose,
            MigrationIdentity::numeric(MigrationAdapter::Goose, 1),
            MigrationIdentity::numeric(MigrationAdapter::Goose, 3),
        ),
        (
            Adapter::Supabase,
            MigrationAdapter::Supabase,
            MigrationIdentity::supabase_version("01").unwrap(),
            MigrationIdentity::supabase_version("3").unwrap(),
        ),
    ] {
        let repo = TestRepo::new().unwrap();
        let goose_up = b"-- +goose Up\nSELECT 1;\n";
        let source = if kind == MigrationAdapter::Goose {
            &goose_up[..]
        } else {
            b"SELECT 1;"
        };
        if kind == MigrationAdapter::Supabase {
            repo.write("migrations/01_a.sql", source);
        }
        repo.write("migrations/1_b.sql", source);
        repo.write("migrations/2_c.sql", source);
        let discovery = discover(&repo, adapter);
        let rows = [applied, missing]
            .into_iter()
            .map(|identity| NativeHistoryRow {
                identity,
                direction: MigrationDirection::Forward,
                rank: None,
                state: NativeHistoryState::Applied,
                checksum: None,
            })
            .collect();
        let history = evidence(
            &discovery,
            kind,
            NativeHistoryCoverage::Complete,
            rows,
            vec![],
        );
        let result = reconcile_native_history(&discovery, &target(), &history).unwrap();

        let expected: &[AppliedStatus] = if kind == MigrationAdapter::Goose {
            &[Applied, Pending, Missing]
        } else {
            &[Applied, Pending, Pending, Missing]
        };
        assert_eq!(statuses(&result), expected);
        assert!(matches!(
            result.applied_history.basis,
            AppliedHistoryBasis::Native {
                method: NativeHistoryMethod::PresenceOnly,
                ..
            }
        ));
        assert_eq!(result.coverage.status(), CoverageStatus::Complete);
        assert!(discovery.plan.integrity.is_none());
    }
}
