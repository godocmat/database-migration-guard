use std::{collections::BTreeMap, path::Path};

use dmg_core::{
    coverage::{Coverage, CoverageGapKind, CoverageStatus, DiagnosticScope},
    digest::ContentDigest,
    migration::{
        ExecutionMode, MigrationAdapter, MigrationDirection, MigrationFile, MigrationIdentity,
        MigrationPlan,
    },
    risk::ObjectKind,
    source::{ByteRange, RepoPath},
    target::PostgresTarget,
};
use dmg_discovery::{Adapter, DiscoveryLimits, DiscoveryOptions, SqlxLayout, discover_worktree};
use dmg_postgres::{
    AlterChange, ConstraintKind, DefaultExpr, IndexKey, Operation, Predicate, RelationKind,
    Transaction, parse_migration, parse_plan,
};

fn input(name: &str) -> Vec<u8> {
    std::fs::read(
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../fixtures/dmg-02/inputs")
            .join(name),
    )
    .unwrap()
}

fn file(version: i64, path: &str, input: &[u8]) -> MigrationFile {
    MigrationFile {
        identity: MigrationIdentity {
            adapter: MigrationAdapter::Plain,
            version: version.into(),
        },
        path: RepoPath::new(path).unwrap(),
        direction: MigrationDirection::Forward,
        digest: ContentDigest::of(input),
        source_segments: vec![ByteRange::new(0, input.len()).unwrap()],
        execution: ExecutionMode::PSQL_FILE_TRANSACTION,
    }
}

fn parse_fixture(name: &str) -> dmg_postgres::ParsedMigration {
    let sql = input(name);
    parse_migration(
        &file(1, "migrations/01_fixture.sql", &sql),
        &sql,
        PostgresTarget::V18,
    )
}

fn path_gap_kind(coverage: &Coverage) -> CoverageGapKind {
    let [gap] = coverage.gaps() else {
        panic!("expected one gap, got {coverage:#?}")
    };
    gap.kind
}

#[test]
fn reversible_sqlx_keeps_exact_lf_and_crlf_coordinates() {
    for (name, expected_statement, expected_action) in [
        ("spans-lf.sql", 87, 139..162),
        ("spans-crlf.sql", 88, 142..165),
    ] {
        let sql = input(name);
        let repo = tempfile::tempdir().unwrap();
        let root = repo.path().join("migrations");
        std::fs::create_dir(&root).unwrap();
        std::fs::write(root.join("002_spans.up.sql"), &sql).unwrap();
        std::fs::write(root.join("002_spans.down.sql"), input("spans-down.sql")).unwrap();
        let options = DiscoveryOptions::new(
            vec![RepoPath::new("migrations").unwrap()],
            Adapter::Sqlx(SqlxLayout::Reversible),
            DiscoveryLimits::default(),
        )
        .unwrap();
        let discovery = discover_worktree(repo.path(), &options);
        let plan = parse_plan(&discovery.plan, PostgresTarget::V17, |path| {
            discovery.source_bytes(path)
        });
        assert_eq!(plan.coverage, Coverage::Complete, "{name}: {plan:#?}");
        let [migration] = plan.migrations.as_slice() else {
            panic!("only the Up should be parsed")
        };
        assert_eq!(migration.execution, ExecutionMode::SQLX_NO_TRANSACTION);
        assert_eq!(migration.path.as_str(), "migrations/002_spans.up.sql");
        let [first, second] = migration.statements.as_slice() else {
            panic!("{migration:#?}")
        };
        assert_eq!(second.span.bytes.start(), expected_statement);
        assert_eq!(
            (second.span.start.line.get(), second.span.start.column.get()),
            (2, 66)
        );
        let Operation::AlterTable(first_alter) = &first.operation else {
            panic!("{first:#?}")
        };
        let AlterChange::AddColumn { column, .. } = &first_alter.actions[0].change else {
            panic!("{first_alter:#?}")
        };
        assert_eq!(column.default, DefaultExpr::Constant);
        let Operation::AlterTable(alter) = &second.operation else {
            panic!("{second:#?}")
        };
        let drop = &alter.actions[1];
        assert!(
            matches!(&drop.change, AlterChange::DropColumn { name, .. } if name == "legacy_code")
        );
        assert_eq!(drop.span.bytes.as_range(), expected_action);
        assert_eq!(
            (drop.span.start.line.get(), drop.span.start.column.get()),
            (4, 3)
        );
        assert_eq!(&sql[drop.span.bytes.as_range()], b"DROP COLUMN legacy_code");
    }
}

#[test]
fn spans_map_nonzero_source_segments_and_quoted_names() {
    let sql = include_bytes!("../../../spikes/dmg-01-parser/fixtures/quoted.sql");
    let mut migration = file(1, "migrations/01_quoted.sql", sql);
    migration.source_segments = vec![ByteRange::new(30, sql.len()).unwrap()];
    let parsed = parse_migration(&migration, sql, PostgresTarget::V17);
    assert_eq!(parsed.coverage, Coverage::Complete, "{parsed:#?}");
    let [alter, select] = parsed.statements.as_slice() else {
        panic!("{parsed:#?}")
    };
    assert_eq!(alter.span.bytes.as_range(), 30..141);
    assert_eq!(select.span.bytes.as_range(), 154..166);
    let Operation::AlterTable(table) = &alter.operation else {
        panic!("{alter:#?}")
    };
    assert_eq!(table.relation.schema.as_deref(), Some("App"));
    assert_eq!(table.relation.name, "café");
    assert!(table.if_exists);
    let [add, drop] = table.actions.as_slice() else {
        panic!("{table:#?}")
    };
    assert!(
        matches!(&add.change, AlterChange::AddColumn { column, if_not_exists: true } if column.name == "Notes")
    );
    assert!(
        matches!(&drop.change, AlterChange::DropColumn { name, if_exists: true, cascade: false } if name == "Old")
    );
    assert_eq!(
        (drop.span.bytes.start(), drop.span.start.column.get()),
        (105, 75)
    );
}

#[test]
fn distinguishes_target_syntax_and_requires_context_for_partitioned_fk() {
    let baseline = input("base.sql");
    let named_nn = input("named-not-null.sql");
    let partitioned_fk = input("partitioned-fk.sql");
    let base = file(1, "migrations/001_base.sql", &baseline);
    for (sql, case) in [(&named_nn, "not-null"), (&partitioned_fk, "foreign-key")] {
        let candidate = file(2, "migrations/002_change.sql", sql);
        let plan = MigrationPlan {
            inventory: vec![base.clone(), candidate.clone()],
            selected_forwards: Some(vec![candidate.path.clone()]),
            integrity: None,
            applied_history: None,
            coverage: Coverage::Complete,
        };
        let sources = BTreeMap::from([
            (base.path.clone(), baseline.as_slice()),
            (candidate.path.clone(), sql.as_slice()),
        ]);
        for target in [
            PostgresTarget::V15,
            PostgresTarget::V16,
            PostgresTarget::V17,
            PostgresTarget::V18,
        ] {
            let parsed = parse_plan(&plan, target, |path| sources.get(path).copied());
            assert_eq!(parsed.selected_forwards, plan.selected_forwards);
            assert_eq!(parsed.migrations.len(), 2);
            assert_eq!(
                parsed.migrations[0].coverage,
                Coverage::Complete,
                "{parsed:#?}"
            );
            assert!(parsed.migrations[0].statements.iter().any(|statement| {
                matches!(&statement.operation, Operation::CreateTable(table) if table.relation.name == "events" && table.kind == RelationKind::PartitionedTable)
            }));
            let [statement] = parsed.migrations[1].statements.as_slice() else {
                panic!("{parsed:#?}")
            };
            let Operation::AlterTable(alter) = &statement.operation else {
                panic!("{statement:#?}")
            };
            let AlterChange::AddConstraint(constraint) = &alter.actions[0].change else {
                panic!("{alter:#?}")
            };
            assert!(
                constraint.not_valid && constraint.enforced,
                "{constraint:#?}"
            );
            match case {
                "not-null" => {
                    assert_eq!(constraint.kind, ConstraintKind::NotNull);
                    assert_eq!(constraint.columns, ["email"]);
                }
                _ => {
                    let ConstraintKind::ForeignKey {
                        referenced_table, ..
                    } = &constraint.kind
                    else {
                        panic!("{constraint:#?}")
                    };
                    assert_eq!(referenced_table.name, "accounts");
                }
            }
            if target < PostgresTarget::V18 {
                assert_eq!(
                    path_gap_kind(&parsed.coverage),
                    CoverageGapKind::IncompatibleTarget
                );
                let DiagnosticScope::Span { span } = &parsed.coverage.gaps()[0].scope else {
                    panic!("{parsed:#?}")
                };
                assert_eq!(span, &alter.actions[0].span);
            } else {
                assert_eq!(parsed.coverage, Coverage::Complete, "{parsed:#?}");
            }
        }
    }
    let standalone = file(1, "migrations/001_fk.sql", &partitioned_fk);
    let parsed = parse_migration(&standalone, &partitioned_fk, PostgresTarget::V17);
    assert_eq!(
        path_gap_kind(&parsed.coverage),
        CoverageGapKind::UnresolvedDependency
    );
}

#[test]
fn preserves_findings_context_when_other_statements_are_incomplete() {
    let sql = include_bytes!("../../../spikes/dmg-01-parser/fixtures/broken.sql");
    let parsed = parse_migration(
        &file(1, "migrations/001_broken.sql", sql),
        sql,
        PostgresTarget::V18,
    );
    assert_eq!(path_gap_kind(&parsed.coverage), CoverageGapKind::ParseError);
    let DiagnosticScope::Span { span } = &parsed.coverage.gaps()[0].scope else {
        panic!("{parsed:#?}")
    };
    assert_eq!(
        &sql[span.bytes.as_range()],
        b"ALTER TABLE t ADD COLUMN broken"
    );
    assert!(matches!(&parsed.statements[0].operation, Operation::Select));
    assert!(
        matches!(&parsed.statements[1].operation, Operation::Drop(drop) if drop.kind == ObjectKind::Relation)
    );
    let unsupported = parse_fixture("dynamic-do.sql");
    assert_eq!(
        path_gap_kind(&unsupported.coverage),
        CoverageGapKind::UnsupportedSql
    );
    assert!(matches!(
        unsupported.statements[0].operation,
        Operation::Unsupported
    ));
    let malformed = parse_fixture("malformed.sql");
    assert_eq!(
        path_gap_kind(&malformed.coverage),
        CoverageGapKind::ParseError
    );
}

#[test]
fn normalizes_core_risk_operations_and_spans() {
    let cases = [
        "add-label.sql",
        "add-label-default.sql",
        "add-volatile-default.sql",
        "create-and-index.sql",
        "rename-legacy.sql",
        "truncate-accounts.sql",
        "drop-legacy-index.sql",
        "index-concurrently-explicit.sql",
        "index-concurrently-batch.sql",
        "email-check-valid.sql",
        "email-check-not-valid.sql",
        "set-email-not-null.sql",
        "validate-email.sql",
        "update-all.sql",
        "update-by-key.sql",
    ];
    for name in cases {
        let parsed = parse_fixture(name);
        assert_eq!(parsed.coverage, Coverage::Complete, "{name}: {parsed:#?}");
        match name {
            "add-label.sql" | "add-label-default.sql" | "add-volatile-default.sql" => {
                let Operation::AlterTable(alter) = &parsed.statements[0].operation else {
                    panic!("{parsed:#?}")
                };
                let AlterChange::AddColumn { column, .. } = &alter.actions[0].change else {
                    panic!("{alter:#?}")
                };
                let expected = match name {
                    "add-label.sql" => DefaultExpr::Absent,
                    "add-label-default.sql" => DefaultExpr::Constant,
                    _ => DefaultExpr::Volatile,
                };
                assert_eq!(column.default, expected);
            }
            "create-and-index.sql" => {
                let Operation::CreateTable(table) = &parsed.statements[0].operation else {
                    panic!("{parsed:#?}")
                };
                assert_eq!(table.kind, RelationKind::Table);
                assert!(table.columns[0].not_null);
                let Operation::CreateIndex(index) = &parsed.statements[1].operation else {
                    panic!("{parsed:#?}")
                };
                assert_eq!(index.keys, [IndexKey::Column("flag".into())]);
                assert!(!index.concurrently);
            }
            "rename-legacy.sql" => {
                let Operation::Rename(rename) = &parsed.statements[0].operation else {
                    panic!("{parsed:#?}")
                };
                assert_eq!(rename.member.as_deref(), Some("legacy_code"));
                assert_eq!(rename.new_name, "external_code");
                assert_eq!(
                    &input(name)[rename.action_span.bytes.as_range()],
                    b"RENAME COLUMN legacy_code TO external_code"
                );
            }
            "truncate-accounts.sql" => assert!(
                matches!(&parsed.statements[0].operation, Operation::Truncate(truncate) if truncate.relations[0].name == "accounts")
            ),
            "drop-legacy-index.sql" => assert!(
                matches!(&parsed.statements[0].operation, Operation::Drop(drop) if drop.kind == ObjectKind::Index && drop.objects[0].name == "accounts_legacy_idx")
            ),
            "index-concurrently-explicit.sql" => {
                assert!(matches!(
                    parsed.statements[0].operation,
                    Operation::Transaction(Transaction::Begin)
                ));
                assert!(
                    matches!(&parsed.statements[1].operation, Operation::CreateIndex(index) if index.concurrently)
                );
                assert!(matches!(
                    parsed.statements[2].operation,
                    Operation::Transaction(Transaction::Commit { chain: false })
                ));
            }
            "index-concurrently-batch.sql" => {
                assert_eq!(parsed.statements.len(), 2);
                assert!(
                    matches!(&parsed.statements[1].operation, Operation::CreateIndex(index) if index.concurrently)
                );
            }
            "email-check-valid.sql" | "email-check-not-valid.sql" => {
                let Operation::AlterTable(alter) = &parsed.statements[0].operation else {
                    panic!("{parsed:#?}")
                };
                let AlterChange::AddConstraint(constraint) = &alter.actions[0].change else {
                    panic!("{alter:#?}")
                };
                assert_eq!(
                    constraint.kind,
                    ConstraintKind::Check {
                        proves_not_null: vec!["email".into()]
                    }
                );
                assert_eq!(constraint.not_valid, name == "email-check-not-valid.sql");
                assert!(constraint.enforced);
            }
            "set-email-not-null.sql" => {
                let Operation::AlterTable(alter) = &parsed.statements[0].operation else {
                    panic!("{parsed:#?}")
                };
                assert!(
                    matches!(&alter.actions[0].change, AlterChange::SetNotNull { column } if column == "email")
                );
            }
            "validate-email.sql" => {
                let Operation::AlterTable(alter) = &parsed.statements[0].operation else {
                    panic!("{parsed:#?}")
                };
                assert!(
                    matches!(&alter.actions[0].change, AlterChange::ValidateConstraint { name } if name == "accounts_email_present")
                );
            }
            "update-all.sql" | "update-by-key.sql" => {
                let Operation::Update(update) = &parsed.statements[0].operation else {
                    panic!("{parsed:#?}")
                };
                assert_eq!(
                    update.predicate,
                    if name == "update-all.sql" {
                        Predicate::Absent
                    } else {
                        Predicate::Filtered
                    }
                );
            }
            _ => unreachable!(),
        }
    }
}

#[test]
fn captures_conservative_expression_facts() {
    let sql = b"ALTER TABLE app.t ADD c text DEFAULT 'x'::text, ADD n timestamp DEFAULT CURRENT_TIMESTAMP, ALTER c TYPE varchar(20) USING c::varchar; DELETE FROM app.t WHERE true OR id = 1; DELETE FROM app.t WHERE id = 1; INSERT INTO app.t (c) VALUES ('a');";
    let parsed = parse_migration(
        &file(1, "migrations/001_expressions.sql", sql),
        sql,
        PostgresTarget::V18,
    );
    assert_eq!(parsed.coverage, Coverage::Complete, "{parsed:#?}");
    let [alter, broad, filtered, insert] = parsed.statements.as_slice() else {
        panic!("{parsed:#?}")
    };
    let Operation::AlterTable(alter) = &alter.operation else {
        panic!("{alter:#?}")
    };
    assert!(
        matches!(&alter.actions[0].change, AlterChange::AddColumn { column, .. } if column.default == DefaultExpr::Constant)
    );
    assert!(
        matches!(&alter.actions[1].change, AlterChange::AddColumn { column, .. } if column.default == DefaultExpr::Stable)
    );
    assert!(
        matches!(&alter.actions[2].change, AlterChange::ChangeType { data_type, using_expression: true, .. } if data_type.integer_modifiers == [Some(20)])
    );
    assert!(
        matches!(&broad.operation, Operation::Delete(change) if change.predicate == Predicate::AlwaysTrue)
    );
    assert!(
        matches!(&filtered.operation, Operation::Delete(change) if change.predicate == Predicate::Filtered)
    );
    assert!(matches!(&insert.operation, Operation::Insert(relation) if relation.name == "t"));
}

#[test]
fn refuses_mismatched_source_and_reports_bounded_input() {
    let good = b"SELECT 1;";
    let bad = b"SELECT 2;";
    let declared = file(1, "migrations/001_input.sql", good);
    let mismatch = parse_migration(&declared, bad, PostgresTarget::V17);
    assert_eq!(
        path_gap_kind(&mismatch.coverage),
        CoverageGapKind::UnreadableInput
    );
    let invalid = [b'-', 0xff];
    let parsed = parse_migration(
        &file(1, "migrations/001_input.sql", &invalid),
        &invalid,
        PostgresTarget::V18,
    );
    assert_eq!(
        path_gap_kind(&parsed.coverage),
        CoverageGapKind::UnsupportedInput
    );
    let too_large = vec![b' '; 256 * 1024 + 1];
    let parsed = parse_migration(
        &file(1, "migrations/001_input.sql", &too_large),
        &too_large,
        PostgresTarget::V18,
    );
    assert_eq!(
        path_gap_kind(&parsed.coverage),
        CoverageGapKind::InputLimitExceeded
    );
    let too_deep = format!("SELECT {}1{};", "(".repeat(65), ")".repeat(65));
    let parsed = parse_migration(
        &file(1, "migrations/001_input.sql", too_deep.as_bytes()),
        too_deep.as_bytes(),
        PostgresTarget::V18,
    );
    assert_eq!(
        path_gap_kind(&parsed.coverage),
        CoverageGapKind::InputLimitExceeded
    );
    assert_eq!(
        parse_migration(&declared, good, PostgresTarget::V18)
            .coverage
            .status(),
        CoverageStatus::Complete
    );
}

#[test]
fn uses_postgresql_boundaries_for_sql_function_blocks() {
    let sql = b"CREATE FUNCTION app.identity_value(n int) RETURNS int LANGUAGE SQL BEGIN ATOMIC SELECT n; END; DROP TABLE app.t;";
    let parsed = parse_migration(
        &file(1, "migrations/001_atomic.sql", sql),
        sql,
        PostgresTarget::V18,
    );
    assert_eq!(
        path_gap_kind(&parsed.coverage),
        CoverageGapKind::UnsupportedSql
    );
    let [function, drop] = parsed.statements.as_slice() else {
        panic!("{parsed:#?}")
    };
    assert!(matches!(function.operation, Operation::Unsupported));
    assert!(matches!(&drop.operation, Operation::Drop(objects) if objects.objects[0].name == "t"));
    assert_eq!(&sql[drop.span.bytes.as_range()], b"DROP TABLE app.t");

    let ambiguous = b"CREATE FUNCTION app.identity_value(n int) RETURNS int LANGUAGE SQL BEGIN ATOMIC SELECT +; END; DROP TABLE app.t;";
    let parsed = parse_migration(
        &file(1, "migrations/001_atomic.sql", ambiguous),
        ambiguous,
        PostgresTarget::V18,
    );
    assert_eq!(path_gap_kind(&parsed.coverage), CoverageGapKind::ParseError);
    assert!(parsed.statements.is_empty());
}

#[test]
fn retains_complete_prefix_before_client_control_or_template() {
    for sql in [
        b"DROP TABLE app.t;\n\\set label 1\nSELECT 2;".as_slice(),
        b"DROP TABLE app.t;\nALTER TABLE {{tenant}}.t ADD COLUMN flag boolean;",
    ] {
        let parsed = parse_migration(
            &file(1, "migrations/001_control.sql", sql),
            sql,
            PostgresTarget::V17,
        );
        assert_eq!(
            path_gap_kind(&parsed.coverage),
            CoverageGapKind::UnsupportedSql
        );
        let [drop] = parsed.statements.as_slice() else {
            panic!("{parsed:#?}")
        };
        assert!(
            matches!(&drop.operation, Operation::Drop(objects) if objects.objects[0].name == "t")
        );
        assert_eq!(&sql[drop.span.bytes.as_range()], b"DROP TABLE app.t");
    }
    let sql = b"DO $example$BEGIN NULL; END;$example$; SELECT 2;";
    let parsed = parse_migration(
        &file(1, "migrations/001_control.sql", sql),
        sql,
        PostgresTarget::V18,
    );
    assert_eq!(
        path_gap_kind(&parsed.coverage),
        CoverageGapKind::UnsupportedSql
    );
    assert!(matches!(parsed.statements[1].operation, Operation::Select));
    let DiagnosticScope::Span { span } = &parsed.coverage.gaps()[0].scope else {
        panic!("{parsed:#?}")
    };
    assert_eq!(
        &sql[span.bytes.as_range()],
        b"$example$BEGIN NULL; END;$example$"
    );
}

#[test]
fn requires_relation_kind_after_conditional_or_rolled_back_creation() {
    let fk = b"ALTER TABLE app.parent ADD CONSTRAINT parent_fk FOREIGN KEY (id) REFERENCES app.ref(id) NOT VALID;";
    for base in [
        b"CREATE TABLE IF NOT EXISTS app.parent (id integer) PARTITION BY RANGE(id);".as_slice(),
        b"BEGIN; CREATE TABLE app.parent (id integer) PARTITION BY RANGE(id); ROLLBACK;",
        b"CREATE TABLE app.parent (id integer) PARTITION BY RANGE(id); SELECT +;",
        b"CREATE TABLE app.parent (id integer, CHECK (id IS NOT NULL) NOT ENFORCED) PARTITION BY RANGE(id);",
    ] {
        let baseline = file(1, "migrations/001_base.sql", base);
        let candidate = file(2, "migrations/002_fk.sql", fk);
        let source = BTreeMap::from([(baseline.path.clone(), base), (candidate.path.clone(), fk)]);
        let plan = MigrationPlan {
            inventory: vec![baseline, candidate.clone()],
            selected_forwards: Some(vec![candidate.path]),
            integrity: None,
            applied_history: None,
            coverage: Coverage::Complete,
        };
        let parsed = parse_plan(&plan, PostgresTarget::V17, |path| source.get(path).copied());
        assert_eq!(path_gap_kind(&parsed.migrations[1].coverage), CoverageGapKind::UnresolvedDependency, "{parsed:#?}");
    }
}

#[test]
fn checks_concurrent_index_eligibility_on_both_supported_majors() {
    let base = input("base.sql");
    for (sql, expected) in [
        (
            b"CREATE INDEX CONCURRENTLY by_id ON app.events(id);".as_slice(),
            CoverageStatus::Incomplete,
        ),
        (
            b"CREATE INDEX CONCURRENTLY by_id ON app.events_1_999(id);",
            CoverageStatus::Complete,
        ),
    ] {
        for target in [PostgresTarget::V17, PostgresTarget::V18] {
            let baseline = file(1, "migrations/001_base.sql", &base);
            let mut candidate = file(2, "migrations/002_idx.sql", sql);
            candidate.execution = ExecutionMode::PSQL_AUTOCOMMIT;
            let source = BTreeMap::from([
                (baseline.path.clone(), base.as_slice()),
                (candidate.path.clone(), sql),
            ]);
            let plan = MigrationPlan {
                inventory: vec![baseline, candidate.clone()],
                selected_forwards: Some(vec![candidate.path]),
                integrity: None,
                applied_history: None,
                coverage: Coverage::Complete,
            };
            let parsed = parse_plan(&plan, target, |path| source.get(path).copied());
            assert_eq!(parsed.coverage.status(), expected, "{parsed:#?}");
            if expected == CoverageStatus::Incomplete {
                assert_eq!(
                    path_gap_kind(&parsed.coverage),
                    CoverageGapKind::IncompatibleTarget
                );
            }
            assert!(
                matches!(&parsed.migrations[1].statements[0].operation, Operation::CreateIndex(index) if index.concurrently)
            );
        }
    }
}

#[test]
fn accepts_empty_input_and_consecutive_delimiters() {
    for (sql, count) in [
        (b"".as_slice(), 0),
        (b" \n\t ", 0),
        (b"; -- comment\n;", 0),
        (b"; SELECT 1; ; SELECT 2;;", 2),
    ] {
        let parsed = parse_migration(
            &file(1, "migrations/001_empty.sql", sql),
            sql,
            PostgresTarget::V17,
        );
        assert_eq!(parsed.coverage, Coverage::Complete, "{parsed:#?}");
        assert_eq!(parsed.statements.len(), count);
        for statement in &parsed.statements {
            let statement_sql = &sql[statement.span.bytes.as_range()];
            assert!(
                statement_sql == b"SELECT 1" || statement_sql == b"SELECT 2",
                "{statement:#?}"
            );
        }
    }
}
