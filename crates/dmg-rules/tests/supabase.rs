use dmg_core::{
    coverage::{Coverage, CoverageGapKind},
    digest::ContentDigest,
    migration::{ExecutionMode, MigrationDirection, MigrationFile, MigrationIdentity},
    risk::RuleId,
    source::{ByteRange, RepoPath},
    target::PostgresTarget,
};
use dmg_postgres::{ParsedMigration, ParsedPlan, parse_migration};
use dmg_rules::{Analysis, Rule, RuleOptions, analyze};

const TARGET: PostgresTarget = PostgresTarget::V18;
const ADD: &str = "ALTER TABLE public.probe ADD COLUMN first_value text;";
const INDEX: &str = "/* leading comment */ CREATE\nUNIQUE INDEX CONCURRENTLY probe_idx ON public.probe (first_value);";
const UNMATCHED_INDEX: &str =
    "CREATE /* internal comment */ INDEX CONCURRENTLY probe_idx ON public.probe (first_value);";
const UPDATE: &str = "UPDATE public.probe SET first_value = 'existing';";

fn parse(version: &str, sql: &str) -> ParsedMigration {
    let input = sql.as_bytes();
    let migration = MigrationFile {
        identity: MigrationIdentity::supabase_version(version).unwrap(),
        path: RepoPath::new(format!("supabase/migrations/{version}_fixture.sql")).unwrap(),
        direction: MigrationDirection::Forward,
        digest: ContentDigest::of(input),
        source_segments: vec![ByteRange::new(0, input.len()).unwrap()],
        execution: ExecutionMode::SUPABASE_BATCH,
    };
    parse_migration(&migration, input, TARGET)
}

fn check(migrations: Vec<ParsedMigration>) -> Analysis {
    let plan = ParsedPlan {
        coverage: Coverage::from_gaps(
            migrations
                .iter()
                .flat_map(|migration| migration.coverage.gaps().iter().cloned())
                .collect(),
        ),
        migrations,
        selected_forwards: None,
    };
    analyze(&plan, TARGET, &RuleOptions::default())
}

fn ids(result: &Analysis) -> Vec<RuleId> {
    result
        .findings
        .iter()
        .map(|finding| finding.rule_id)
        .collect()
}

#[test]
fn pg007_respects_native_index_and_dml_batch_boundaries() {
    for (sql, expected) in [
        (format!("{ADD} {INDEX} {UPDATE}"), vec![Rule::Pg006.id()]),
        (format!("{ADD} {UNMATCHED_INDEX}"), vec![Rule::Pg007.id()]),
        (
            format!("{ADD} BEGIN; {INDEX} COMMIT;"),
            vec![Rule::Pg007.id()],
        ),
        (
            format!("{ADD} {UPDATE}"),
            vec![Rule::Pg006.id(), Rule::Pg007.id()],
        ),
    ] {
        let result = check(vec![parse("1", &sql)]);
        assert_eq!(result.coverage, Coverage::Complete, "{sql:?}: {result:#?}");
        assert_eq!(ids(&result), expected, "{sql:?}: {result:#?}");
        if expected == [Rule::Pg007.id()] {
            let finding = &result.findings[0];
            assert!(finding.explanation.contains("cannot run inside"));
            assert!(sql[finding.span.bytes.as_range()].starts_with("CREATE"));
        }
    }
}

#[test]
fn an_open_explicit_transaction_prevents_proving_later_connection_state() {
    let first = parse("1", &format!("BEGIN; {ADD}"));
    let later = parse("2", INDEX);
    let result = check(vec![first, later]);
    assert!(result.findings.is_empty(), "{result:#?}");
    assert!(
        result.coverage.gaps().iter().any(|gap| {
            gap.kind == CoverageGapKind::UnresolvedTransactionMode
                && gap.message.contains("leaves an explicit transaction open")
        }),
        "{result:#?}",
    );
    assert!(
        result.coverage.gaps().iter().any(|gap| {
            gap.kind == CoverageGapKind::UnresolvedTransactionMode
                && gap
                    .message
                    .contains("could not establish the execution block")
        }),
        "{result:#?}",
    );
}
