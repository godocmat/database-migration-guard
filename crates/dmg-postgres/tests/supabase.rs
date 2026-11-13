use std::fs;

use dmg_core::{
    coverage::{Coverage, CoverageGapKind, DiagnosticScope},
    digest::ContentDigest,
    migration::{ExecutionMode, MigrationDirection, StatementDelivery},
    source::RepoPath,
    target::PostgresTarget,
};
use dmg_discovery::{Adapter, Discovery, DiscoveryLimits, DiscoveryOptions, discover_worktree};
use dmg_postgres::{ParsedPlan, parse_plan};

const ROOT: &str = "supabase/migrations";

fn project(files: &[(&str, &[u8])]) -> Discovery {
    let repo = tempfile::tempdir().unwrap();
    let root = repo.path().join(ROOT);
    fs::create_dir_all(&root).unwrap();
    for (filename, bytes) in files {
        let path = root.join(filename);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, bytes).unwrap();
    }
    let options = DiscoveryOptions::new(
        vec![RepoPath::new(ROOT).unwrap()],
        Adapter::Supabase,
        DiscoveryLimits::default(),
    )
    .unwrap();
    discover_worktree(repo.path(), &options)
}

fn parse(discovery: &Discovery, target: PostgresTarget) -> ParsedPlan {
    parse_plan(&discovery.plan, target, |path| discovery.source_bytes(path))
}

#[test]
fn discovers_native_lexical_versions_and_reports_ambiguous_inputs() {
    let sql = b"SELECT 1;";
    let discovery = project(&[
        ("2_looks.down.sql", sql),
        ("1_second.sql", sql),
        ("10_third.sql", sql),
        ("01_first.sql", sql),
        ("3_ignored.SQL", b"not SQL"),
        ("archive/4_hidden.sql", b"not SQL"),
        ("README.sql", b"not SQL"),
    ]);
    assert_eq!(discovery.plan.coverage, Coverage::Complete);
    assert_eq!(
        discovery
            .plan
            .inventory
            .iter()
            .map(|file| file.identity.version.to_string())
            .collect::<Vec<_>>(),
        ["01", "1", "10", "2"],
    );
    assert!(discovery.plan.inventory.iter().all(|file| {
        file.direction == MigrationDirection::Forward && file.digest == ContentDigest::of(sql)
    }),);
    assert_eq!(discovery.plan.selected_forwards.as_ref().unwrap().len(), 4);

    let duplicate = project(&[("01_first.sql", sql), ("01_duplicate.sql", sql)]);
    assert!(duplicate.plan.selected_forwards.is_none());
    assert_eq!(
        duplicate.plan.coverage.gaps()[0].kind,
        CoverageGapKind::AmbiguousIdentity,
    );

    let legacy = project(&[
        ("20211208000000_init.sql", sql),
        ("20211209000000_init.sql", sql),
        ("!readme", b"ignored"),
    ]);
    assert!(legacy.plan.selected_forwards.is_none());
    assert_eq!(legacy.plan.inventory.len(), 1);
    let [gap] = legacy.plan.coverage.gaps() else {
        panic!("{:#?}", legacy.plan.coverage);
    };
    assert_eq!(gap.kind, CoverageGapKind::UnsupportedInput);
    assert_eq!(
        gap.scope,
        DiagnosticScope::Path {
            path: RepoPath::new("supabase/migrations/20211208000000_init.sql").unwrap(),
        },
    );
}

#[test]
fn directive_uses_the_original_first_line_and_keeps_bom_and_crlf_coordinates() {
    for (sql, expected, expected_position) in [
        (
            "\u{feff}-- pg-delta: transaction=false\r\nSELECT 'café';\r\n",
            ExecutionMode::PSQL_AUTOCOMMIT,
            (2, 1),
        ),
        (
            "-- pg-delta: transaction=false\nSELECT 'café';",
            ExecutionMode::PSQL_AUTOCOMMIT,
            (2, 1),
        ),
        (
            "\n-- pg-delta: transaction=false\nSELECT 'café';",
            ExecutionMode::SUPABASE_BATCH,
            (3, 1),
        ),
        (
            "-- pg-delta: transaction=false \r\nSELECT 'café';",
            ExecutionMode::SUPABASE_BATCH,
            (2, 1),
        ),
        (
            "\u{feff}SELECT 'café';",
            ExecutionMode::SUPABASE_BATCH,
            (1, 2),
        ),
    ] {
        let bytes = sql.as_bytes();
        let discovery = project(&[("1_directive.sql", bytes)]);
        let [file] = discovery.plan.inventory.as_slice() else {
            panic!("{:#?}", discovery.plan);
        };
        assert_eq!(file.execution, expected);
        assert_eq!(file.digest, ContentDigest::of(bytes));
        assert_eq!(discovery.source_bytes(&file.path), Some(bytes));
        let plan = parse(&discovery, PostgresTarget::V18);
        assert_eq!(plan.coverage, Coverage::Complete, "{sql:?}");
        let [statement] = plan.migrations[0].statements.as_slice() else {
            panic!("{plan:#?}");
        };
        assert_eq!(statement.delivery, expected.statement_delivery);
        assert_eq!(&sql[statement.span.bytes.as_range()], "SELECT 'café'");
        assert_eq!(
            (
                statement.span.start.line.get(),
                statement.span.start.column.get()
            ),
            expected_position,
        );
    }
}

#[test]
fn resolves_native_splits_and_marks_unverified_boundary_effects() {
    let batch = StatementDelivery::ExtendedBatch;
    let standalone = StatementDelivery::PerStatement;
    let index = "CREATE INDEX CONCURRENTLY probe_idx ON public.probe (id);";
    let unmatched = "CREATE/* internal */ INDEX CONCURRENTLY probe_idx ON public.probe (id);";
    let alter = "ALTER TABLE public.probe ADD COLUMN first_value text;";
    for target in [PostgresTarget::V17, PostgresTarget::V18] {
        for (sql, expected, gap) in [
            (
                format!(
                    "{alter} /* leading ; /* nested */ trivia */ CREATE\nUNIQUE INDEX CONCURRENTLY probe_idx ON public.probe(id); SELECT 1;"
                ),
                vec![(0, batch), (1, standalone), (2, batch)],
                false,
            ),
            (
                format!("{index} DROP INDEX CONCURRENTLY public.probe_idx; SELECT 1;"),
                vec![(0, standalone), (1, standalone), (2, batch)],
                false,
            ),
            (
                format!("{alter} {unmatched}"),
                vec![(0, batch), (0, batch)],
                false,
            ),
            (unmatched.into(), vec![(0, batch)], true),
            (
                format!("{alter} /* leading ; trivia */ BEGIN; {index} COMMIT;"),
                vec![(0, standalone); 4],
                false,
            ),
            (
                format!("{alter} BEGIN/* adjacent ; trivia */;"),
                vec![(0, batch), (0, batch)],
                true,
            ),
            (
                format!("{alter} SAVEPOINT boundary; ROLLBACK TO SAVEPOINT boundary;"),
                vec![(0, batch); 3],
                false,
            ),
        ] {
            let discovery = project(&[("1_mode.sql", sql.as_bytes())]);
            let plan = parse(&discovery, target);
            let [migration] = plan.migrations.as_slice() else {
                panic!("{plan:#?}");
            };
            assert_eq!(
                migration
                    .statements
                    .iter()
                    .map(|stmt| (stmt.segment, stmt.delivery))
                    .collect::<Vec<_>>(),
                expected,
                "{sql:?}",
            );
            if gap {
                assert!(
                    plan.coverage.gaps().iter().any(|gap| {
                        gap.kind == CoverageGapKind::UnresolvedTransactionMode
                            && matches!(gap.scope, DiagnosticScope::Span { .. })
                    }),
                    "{sql:?}: {plan:#?}"
                );
            } else {
                assert_eq!(plan.coverage, Coverage::Complete, "{sql:?}: {plan:#?}");
            }
        }
    }
}
