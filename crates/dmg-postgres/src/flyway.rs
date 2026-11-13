use dmg_core::{
    coverage::{CoverageGap, CoverageGapKind},
    migration::{FlywaySettings, TransactionWrapper},
    risk::ObjectKind,
};

use crate::{Operation, ParsedMigration, parse::path_gap};

pub(crate) fn apply_execution(
    settings: FlywaySettings,
    source: &str,
    migration: &mut ParsedMigration,
    gaps: &mut Vec<CoverageGap>,
) {
    // Flyway substitutes placeholders before it parses SQL, including text in comments.
    if source.contains("${") {
        gaps.push(path_gap(
            &migration.path,
            CoverageGapKind::UnsupportedInput,
            "Flyway SQL placeholder substitution is outside the supported profile",
        ));
        return;
    }
    if !gaps.is_empty() {
        // An unclassified statement can change the wrapper for the entire file.
        return;
    }

    if migration
        .statements
        .iter()
        .any(|statement| matches!(statement.operation, Operation::Transaction(_)))
    {
        gaps.push(path_gap(
            &migration.path,
            CoverageGapKind::UnresolvedTransactionMode,
            "Flyway JDBC execution with authored transaction commands is outside the supported profile",
        ));
        return;
    }

    let concurrent = migration
        .statements
        .iter()
        .filter(|statement| concurrent_index(&statement.operation))
        .count();
    let issue = if settings.execute_in_transaction
        && !settings.mixed
        && (1..migration.statements.len()).contains(&concurrent)
    {
        Some(
            "Flyway rejects this mix of transactional statements and concurrent index operations with mixed=false",
        )
    } else if concurrent > 0 && settings.transactional_lock {
        Some(
            "Flyway's transactional PostgreSQL advisory lock can prevent concurrent index operations",
        )
    } else {
        None
    };
    if let Some(message) = issue {
        gaps.push(path_gap(
            &migration.path,
            CoverageGapKind::UnresolvedTransactionMode,
            message,
        ));
        return;
    }

    migration.execution.transaction_wrapper = if settings.execute_in_transaction && concurrent == 0
    {
        TransactionWrapper::File
    } else {
        TransactionWrapper::None
    };
}

fn concurrent_index(operation: &Operation) -> bool {
    match operation {
        Operation::CreateIndex(index) => index.concurrently,
        Operation::Drop(drop) => drop.kind == ObjectKind::Index && drop.concurrently,
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use dmg_core::{
        coverage::{CoverageStatus, DiagnosticScope},
        migration::StatementDelivery,
        source::RepoPath,
        target::PostgresTarget,
    };
    use dmg_discovery::{Adapter, DiscoveryLimits, DiscoveryOptions, discover_worktree};

    use crate::{ParsedMigration, parse_plan};

    use super::*;

    const INDEX: &str = "CREATE /* token gap */ UNIQUE\n INDEX CONCURRENTLY idx ON app.items (id);";
    const ALTER: &str = "ALTER TABLE app.items ADD COLUMN added text;";

    fn parse(sql: &str, settings: FlywaySettings, target: PostgresTarget) -> ParsedMigration {
        let temp = tempfile::tempdir().unwrap();
        let dir = temp.path().join("sql");
        std::fs::create_dir(&dir).unwrap();
        std::fs::write(dir.join("V1__probe.sql"), sql).unwrap();
        let options = DiscoveryOptions::new(
            vec![RepoPath::new("sql").unwrap()],
            Adapter::Flyway(settings),
            DiscoveryLimits::default(),
        )
        .unwrap();
        let discovered = discover_worktree(temp.path(), &options);
        let parsed = parse_plan(&discovered.plan, target, |path| {
            discovered.source_bytes(path)
        });
        let [migration]: [ParsedMigration; 1] = parsed.migrations.try_into().unwrap();
        migration
    }

    #[test]
    fn resolves_verified_native_wrappers_on_both_targets() {
        let default = FlywaySettings::default();
        let session_lock = FlywaySettings {
            transactional_lock: false,
            ..default
        };
        let mixed = FlywaySettings {
            mixed: true,
            ..session_lock
        };
        let unwrapped = FlywaySettings {
            execute_in_transaction: false,
            ..session_lock
        };
        let cases = [
            (ALTER.to_owned(), default, TransactionWrapper::File),
            (ALTER.to_owned(), unwrapped, TransactionWrapper::None),
            (INDEX.to_owned(), session_lock, TransactionWrapper::None),
            (
                "DROP /* token gap */ INDEX CONCURRENTLY IF EXISTS app.idx;".into(),
                session_lock,
                TransactionWrapper::None,
            ),
            (format!("{ALTER}\n{INDEX}"), mixed, TransactionWrapper::None),
            (
                format!("{ALTER}\n{INDEX}"),
                unwrapped,
                TransactionWrapper::None,
            ),
        ];
        for target in [PostgresTarget::V17, PostgresTarget::V18] {
            for (sql, settings, wrapper) in &cases {
                let migration = parse(sql, *settings, target);
                assert_eq!(
                    migration.coverage.status(),
                    CoverageStatus::Complete,
                    "{migration:?}"
                );
                assert_eq!(migration.execution.transaction_wrapper, *wrapper);
                assert_eq!(
                    migration.execution.statement_delivery,
                    StatementDelivery::PerStatement
                );
            }
        }
    }

    #[test]
    fn rejects_mixed_default_and_unresolved_lock_or_execution_scope() {
        let default = FlywaySettings::default();
        let session_lock = FlywaySettings {
            transactional_lock: false,
            ..default
        };
        for (sql, settings, message) in [
            (format!("{ALTER}\n{INDEX}"), session_lock, "mixed=false"),
            (INDEX.to_owned(), default, "advisory lock"),
            (
                format!("BEGIN;\n{ALTER}\nCOMMIT;"),
                default,
                "authored transaction",
            ),
        ] {
            let migration = parse(&sql, settings, PostgresTarget::V17);
            assert_eq!(migration.coverage.status(), CoverageStatus::Incomplete);
            assert_eq!(
                migration.execution.transaction_wrapper,
                TransactionWrapper::Flyway(settings)
            );
            assert!(migration.coverage.gaps().iter().any(|gap| {
                gap.kind == CoverageGapKind::UnresolvedTransactionMode
                    && matches!(&gap.scope, DiagnosticScope::Path { path } if path == &migration.path)
                    && gap.message.contains(message)
            }));
        }
    }

    #[test]
    fn retains_original_spans_and_detects_placeholders_before_execution() {
        let settings = FlywaySettings::default();
        let sql = format!("\u{feff}-- café 🧪\r\n{ALTER}\r\n");
        let migration = parse(&sql, settings, PostgresTarget::V18);
        assert_eq!(migration.coverage.status(), CoverageStatus::Complete);
        let location = &migration.statements[0].span;
        assert_eq!(&sql[location.bytes.as_range()], ALTER.trim_end_matches(';'));
        assert_eq!(location.start.line.get(), 2);
        assert_eq!(location.start.column.get(), 1);

        let sql = format!("{ALTER}\n{}", "SELECT '${flyway:timestamp}';");
        let migration = parse(&sql, settings, PostgresTarget::V18);
        assert_eq!(
            migration.execution.transaction_wrapper,
            TransactionWrapper::Flyway(settings)
        );
        assert_eq!(migration.statements.len(), 2);
        assert!(migration.coverage.gaps().iter().any(|gap| {
            gap.kind == CoverageGapKind::UnsupportedInput && gap.message.contains("placeholder")
        }));
    }

    #[test]
    fn unknown_late_sql_retains_earlier_source_without_guessing_the_wrapper() {
        let settings = FlywaySettings::default();
        let sql = format!("{ALTER}\nVACUUM app.items;");
        let migration = parse(&sql, settings, PostgresTarget::V18);
        assert_eq!(
            migration.execution.transaction_wrapper,
            TransactionWrapper::Flyway(settings)
        );
        assert!(matches!(
            migration.statements[0].operation,
            Operation::AlterTable(_)
        ));
        assert!(
            migration
                .coverage
                .gaps()
                .iter()
                .any(|gap| gap.kind == CoverageGapKind::UnsupportedSql)
        );
    }
}
