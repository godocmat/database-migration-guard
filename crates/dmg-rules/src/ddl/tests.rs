use dmg_core::{
    coverage::{Coverage, CoverageStatus},
    digest::ContentDigest,
    migration::{
        ExecutionMode, MigrationAdapter, MigrationDirection, MigrationFile, MigrationIdentity,
        MigrationPlan,
    },
    source::{ByteRange, RepoPath},
};
use dmg_postgres::parse_plan;

use super::*;
use crate::transaction::timelines;

const BASE: &str = include_str!("../../../../fixtures/dmg-02/inputs/base.sql");
const SET_NOT_NULL: &str =
    include_str!("../../../../fixtures/dmg-02/inputs/set-email-not-null.sql");

fn analyze(target: PostgresTarget, sources: &[(&str, &str, ExecutionMode)]) -> crate::Analysis {
    let inventory = sources
        .iter()
        .enumerate()
        .map(|(index, (name, sql, execution))| MigrationFile {
            identity: MigrationIdentity {
                adapter: if execution.statement_delivery
                    == dmg_core::migration::StatementDelivery::SingleQuery
                {
                    MigrationAdapter::Sqlx
                } else {
                    MigrationAdapter::Plain
                },
                version: i64::try_from(index + 1).unwrap().into(),
            },
            path: RepoPath::new(*name).unwrap(),
            direction: MigrationDirection::Forward,
            digest: ContentDigest::of(sql.as_bytes()),
            source_segments: vec![ByteRange::new(0, sql.len()).unwrap()],
            execution: *execution,
        })
        .collect::<Vec<_>>();
    let plan = MigrationPlan {
        selected_forwards: Some(vec![inventory.last().unwrap().path.clone()]),
        inventory,
        integrity: None,
        applied_history: None,
        coverage: Coverage::Complete,
    };
    let parsed = parse_plan(&plan, target, |path| {
        sources
            .iter()
            .find(|(name, _, _)| path.as_str() == *name)
            .map(|(_, sql, _)| sql.as_bytes())
    });
    let mut findings = Vec::new();
    let mut locks = Vec::new();
    let mut gaps = parsed.coverage.gaps().to_vec();
    evaluate(
        &parsed,
        target,
        &RuleOptions::default(),
        &timelines(&parsed),
        &mut findings,
        &mut gaps,
        &mut locks,
    );
    crate::Analysis {
        coverage: Coverage::from_gaps(gaps),
        findings,
        locks,
    }
}

fn candidate(previous: &[&str], sql: &str, execution: ExecutionMode) -> crate::Analysis {
    let paths = [
        "migrations/001_base.sql",
        "migrations/002_prior.sql",
        "migrations/003_prior.sql",
    ];
    let mut sources = previous
        .iter()
        .zip(paths)
        .map(|(&sql, path)| (path, sql, ExecutionMode::PSQL_FILE_TRANSACTION))
        .collect::<Vec<_>>();
    sources.push(("migrations/004_candidate.sql", sql, execution));
    analyze(PostgresTarget::V18, &sources)
}

fn has_rule(analysis: &crate::Analysis, rule: Rule, severity: Severity) -> bool {
    analysis
        .findings
        .iter()
        .any(|finding| finding.rule_id == rule.id() && finding.severity == severity)
}

#[test]
fn public_alter_controls_distinguish_lock_rewrite_and_scan() {
    let file = ExecutionMode::PSQL_FILE_TRANSACTION;
    let add = include_str!("../../../../fixtures/dmg-02/inputs/add-label-default.sql");
    let volatile = include_str!("../../../../fixtures/dmg-02/inputs/add-volatile-default.sql");
    let validated = include_str!("../../../../fixtures/dmg-02/inputs/email-check-valid.sql");
    let deferred = include_str!("../../../../fixtures/dmg-02/inputs/email-check-not-valid.sql");
    let validate = include_str!("../../../../fixtures/dmg-02/inputs/validate-email.sql");

    for (prior, sql, expected, lock) in [
        (vec![BASE], add, None, RelationLock::AccessExclusive),
        (
            vec![BASE],
            volatile,
            Some((Rule::Pg003, Severity::High)),
            RelationLock::AccessExclusive,
        ),
        (
            vec![BASE],
            SET_NOT_NULL,
            Some((Rule::Pg004, Severity::Medium)),
            RelationLock::AccessExclusive,
        ),
        (
            vec![BASE, validated],
            SET_NOT_NULL,
            None,
            RelationLock::AccessExclusive,
        ),
        (
            vec![BASE, deferred],
            validate,
            Some((Rule::Pg004, Severity::Medium)),
            RelationLock::ShareUpdateExclusive,
        ),
    ] {
        let result = candidate(&prior, sql, file);
        assert_eq!(result.coverage.status(), CoverageStatus::Complete, "{sql}");
        assert_eq!(
            result.findings.len(),
            usize::from(expected.is_some()),
            "{sql}"
        );
        if let Some((rule, severity)) = expected {
            assert!(has_rule(&result, rule, severity), "{sql}");
            assert_eq!(result.findings[0].object.as_ref().unwrap().name, "accounts");
        }
        assert_eq!(result.locks.len(), 1, "{sql}");
        assert_eq!(result.locks[0].mode, lock, "{sql}");
        assert_eq!(result.locks[0].object.name, "accounts", "{sql}");
    }
}

#[test]
fn public_destructive_and_rename_actions_use_original_spans() {
    for (source, range) in [
        (
            include_str!("../../../../fixtures/dmg-02/inputs/spans-lf.sql"),
            139..162,
        ),
        (
            include_str!("../../../../fixtures/dmg-02/inputs/spans-crlf.sql"),
            142..165,
        ),
    ] {
        let result = analyze(
            PostgresTarget::V18,
            &[
                (
                    "migrations/001_base.up.sql",
                    BASE,
                    ExecutionMode::SQLX_TRANSACTION,
                ),
                (
                    "migrations/002_spans.up.sql",
                    source,
                    ExecutionMode::SQLX_NO_TRANSACTION,
                ),
            ],
        );
        assert_eq!(result.coverage.status(), CoverageStatus::Complete);
        assert!(has_rule(&result, Rule::Pg001, Severity::Critical));
        let finding = &result.findings[0];
        assert_eq!(finding.span.bytes.as_range(), range);
        assert_eq!(finding.span.path.as_str(), "migrations/002_spans.up.sql");
        assert_eq!(
            source[finding.span.bytes.as_range()].trim(),
            "DROP COLUMN legacy_code"
        );
    }

    let rename = include_str!("../../../../fixtures/dmg-02/inputs/rename-legacy.sql");
    let result = candidate(&[BASE], rename, ExecutionMode::PSQL_FILE_TRANSACTION);
    assert!(has_rule(&result, Rule::Pg002, Severity::High));
    let span = &result.findings[0].span;
    assert_eq!(
        &rename[span.bytes.as_range()],
        "RENAME COLUMN legacy_code TO external_code"
    );
}

#[test]
fn removals_report_compatibility_and_data_loss_with_exact_objects_and_spans() {
    let base = "CREATE TABLE app.widgets (id int, \"old😀\" text);
                CREATE TABLE app.legacy_events (id int);";
    let sql = concat!(
        "ALTER TABLE app.widgets ADD COLUMN replacement text, DROP COLUMN \"old😀\";\r\n",
        "DROP TABLE app.legacy_events;",
    );
    let result = candidate(&[base], sql, ExecutionMode::PSQL_FILE_TRANSACTION);
    assert_eq!(result.coverage.status(), CoverageStatus::Complete);
    assert_eq!(result.findings.len(), 4);

    for (kind, name, member, command, documentation) in [
        (
            ObjectKind::Column,
            "widgets",
            Some("old😀"),
            "DROP COLUMN \"old😀\"",
            "https://www.postgresql.org/docs/18/sql-altertable.html",
        ),
        (
            ObjectKind::Relation,
            "legacy_events",
            None,
            "DROP TABLE app.legacy_events",
            "https://www.postgresql.org/docs/18/sql-droptable.html",
        ),
    ] {
        let object = DatabaseObject {
            kind,
            schema: Some("app".into()),
            name: name.into(),
            member: member.map(str::to_owned),
        };
        let finding = |rule: Rule| {
            result
                .findings
                .iter()
                .find(|finding| {
                    finding.rule_id == rule.id() && finding.object.as_ref() == Some(&object)
                })
                .unwrap()
        };
        let loss = finding(Rule::Pg001);
        let compatibility = finding(Rule::Pg002);
        assert_eq!(loss.severity, Severity::Critical);
        assert_eq!(compatibility.severity, Severity::High);
        assert_eq!(compatibility.confidence, Confidence::High);
        assert_eq!(compatibility.documentation, documentation);
        assert_eq!(compatibility.span, loss.span);
        assert_eq!(&sql[compatibility.span.bytes.as_range()], command);
        assert_ne!(compatibility.fingerprint, loss.fingerprint);
    }
}

#[test]
fn unresolved_removal_identity_has_medium_compatibility_risk() {
    let sql = "DROP TABLE IF EXISTS app.known, app.unresolved;
               ALTER TABLE IF EXISTS app.unknown DROP COLUMN IF EXISTS legacy;";
    let result = candidate(
        &["CREATE TABLE app.known (id int);"],
        sql,
        ExecutionMode::PSQL_FILE_TRANSACTION,
    );
    assert_eq!(result.coverage.status(), CoverageStatus::Complete);
    assert_eq!(result.findings.len(), 6);
    for (name, severity) in [
        ("known", Severity::High),
        ("unresolved", Severity::Medium),
        ("unknown", Severity::Medium),
    ] {
        assert!(result.findings.iter().any(|finding| {
            finding.rule_id == Rule::Pg002.id()
                && finding.severity == severity
                && finding
                    .object
                    .as_ref()
                    .is_some_and(|object| object.name == name)
        }));
    }
}

#[test]
fn private_and_conditionally_absent_objects_have_no_removal_compatibility_finding() {
    let sql = "CREATE TABLE app.transient (id int, obsolete text);
               INSERT INTO app.transient VALUES (1, 'synthetic');
               ALTER TABLE app.transient DROP COLUMN obsolete;
               DROP TABLE app.transient;
               DROP TABLE IF EXISTS app.transient;
               ALTER TABLE app.retained DROP COLUMN IF EXISTS nonexistent;
               TRUNCATE app.retained;";
    let result = candidate(
        &["CREATE TABLE app.retained (id int);"],
        sql,
        ExecutionMode::PSQL_FILE_TRANSACTION,
    );
    assert_eq!(result.coverage.status(), CoverageStatus::Complete);
    assert_eq!(result.findings.len(), 3);
    assert!(result.findings.iter().all(|finding| {
        finding.rule_id == Rule::Pg001.id() && finding.severity == Severity::Critical
    }));
}

#[test]
fn column_removal_uses_identity_at_transaction_entry() {
    let base = "CREATE TABLE app.retained (id int);";
    let existing = "CREATE TABLE app.retained (id int, preview text);";
    let add = "ALTER TABLE app.retained ADD COLUMN IF NOT EXISTS preview text;";
    let drop = "ALTER TABLE app.retained DROP COLUMN preview;";
    let file = ExecutionMode::PSQL_FILE_TRANSACTION;
    let cases = [
        (base, file, format!("{add} {drop}"), 1, 0),
        (
            base,
            ExecutionMode::PSQL_AUTOCOMMIT,
            format!("{add} {drop}"),
            1,
            1,
        ),
        (
            base,
            file,
            format!("SAVEPOINT s; {add} RELEASE s; {drop}"),
            1,
            0,
        ),
        (
            base,
            file,
            format!(
                "SAVEPOINT s; {add} ROLLBACK TO s; ALTER TABLE app.retained DROP COLUMN IF EXISTS preview;"
            ),
            0,
            0,
        ),
        (
            existing,
            file,
            format!("SAVEPOINT s; {drop} {add} ROLLBACK TO s; {drop}"),
            2,
            2,
        ),
    ];
    for (baseline, mode, sql, losses, compatibility) in cases {
        let result = candidate(&[baseline], &sql, mode);
        assert_eq!(result.coverage.status(), CoverageStatus::Complete, "{sql}");
        assert_eq!(result.findings.len(), losses + compatibility, "{sql}");
        for (rule, severity, count) in [
            (Rule::Pg001, Severity::Critical, losses),
            (Rule::Pg002, Severity::High, compatibility),
        ] {
            assert_eq!(
                result
                    .findings
                    .iter()
                    .filter(|finding| finding.rule_id == rule.id() && finding.severity == severity)
                    .count(),
                count,
                "{sql}",
            );
        }
    }
}

#[test]
fn rollback_and_savepoints_restore_only_established_constraint_proof() {
    let file = ExecutionMode::PSQL_FILE_TRANSACTION;
    let check = include_str!("../../../../fixtures/dmg-02/inputs/email-check-valid.sql");
    let drop = "ALTER TABLE app.accounts DROP CONSTRAINT accounts_email_present;";
    let cases = [
        (
            vec![BASE],
            format!("SAVEPOINT s; {check} ROLLBACK TO s; {SET_NOT_NULL}"),
            true,
        ),
        (
            vec![BASE, check],
            format!("SAVEPOINT s; {drop} ROLLBACK TO s; {SET_NOT_NULL}"),
            false,
        ),
        (
            vec![BASE],
            format!("SAVEPOINT s; {check} RELEASE s; {SET_NOT_NULL}"),
            false,
        ),
        (
            vec![BASE, check],
            format!("SAVEPOINT s; {drop} RELEASE s; {SET_NOT_NULL}"),
            true,
        ),
        (
            vec![BASE, check],
            String::from(
                "ALTER TABLE app.accounts ALTER COLUMN email SET NOT NULL, DROP CONSTRAINT accounts_email_present;",
            ),
            true,
        ),
    ];
    for (previous, sql, expect_scan) in cases {
        let result = candidate(&previous, &sql, file);
        assert_eq!(result.coverage.status(), CoverageStatus::Complete, "{sql}");
        assert_eq!(
            result.findings.iter().any(|finding| {
                finding.rule_id == Rule::Pg004.id()
                    && sql[finding.span.bytes.as_range()].contains("SET NOT NULL")
            }),
            expect_scan,
            "{sql}",
        );
    }

    let rolled_back = format!("BEGIN; {check} ROLLBACK;");
    let result = candidate(&[BASE, &rolled_back], SET_NOT_NULL, file);
    assert!(has_rule(&result, Rule::Pg004, Severity::Medium));
}

#[test]
fn a_private_table_is_exempt_from_index_blocking_and_data_loss_is_conservative() {
    let file = ExecutionMode::PSQL_FILE_TRANSACTION;
    let create_and_index = include_str!("../../../../fixtures/dmg-02/inputs/create-and-index.sql");
    let regular_index = include_str!("../../../../fixtures/dmg-02/inputs/index-legacy.sql");
    let truncate = include_str!("../../../../fixtures/dmg-02/inputs/truncate-accounts.sql");
    assert!(!has_rule(
        &candidate(&[BASE], create_and_index, file),
        Rule::Pg005,
        Severity::High
    ));
    assert!(has_rule(
        &candidate(&[BASE], regular_index, file),
        Rule::Pg005,
        Severity::High
    ));
    assert!(has_rule(
        &candidate(&[BASE], truncate, file),
        Rule::Pg001,
        Severity::Critical
    ));
    assert!(has_rule(
        &candidate(&[], "CREATE TABLE app.t (id int); DROP TABLE app.t;", file),
        Rule::Pg001,
        Severity::Low,
    ));
    for (sql, mode) in [
        (
            "CREATE TABLE app.t (id int); INSERT INTO app.t VALUES (1); DROP TABLE app.t;",
            file,
        ),
        (
            "CREATE TABLE IF NOT EXISTS app.t (id int); DROP TABLE app.t;",
            file,
        ),
        (
            "CREATE TABLE app.t (id int); DROP TABLE app.t;",
            ExecutionMode::PSQL_AUTOCOMMIT,
        ),
    ] {
        assert!(
            has_rule(&candidate(&[], sql, mode), Rule::Pg001, Severity::Critical),
            "{sql}"
        );
    }
}

#[test]
fn known_binary_cast_and_deferred_foreign_key_preserve_their_context() {
    let file = ExecutionMode::PSQL_FILE_TRANSACTION;
    let metadata = candidate(
        &[BASE],
        "ALTER TABLE app.accounts ALTER COLUMN legacy_code TYPE varchar;",
        file,
    );
    assert!(!has_rule(&metadata, Rule::Pg003, Severity::High));
    let rewrite = candidate(
        &[BASE],
        "ALTER TABLE app.accounts ALTER COLUMN id TYPE text USING id::text;",
        file,
    );
    assert!(has_rule(&rewrite, Rule::Pg003, Severity::High));
    let fk = include_str!("../../../../fixtures/dmg-02/inputs/partitioned-fk.sql");
    let result = candidate(&[BASE], fk, file);
    assert_eq!(result.coverage.status(), CoverageStatus::Complete);
    assert!(result.findings.is_empty());
    assert_eq!(result.locks.len(), 2);
    assert!(
        result
            .locks
            .iter()
            .all(|lock| lock.mode == RelationLock::ShareRowExclusive)
    );
    assert!(
        result
            .locks
            .iter()
            .any(|lock| lock.object.name == "accounts")
    );
    assert!(result.locks.iter().any(|lock| lock.object.name == "events"));
}
