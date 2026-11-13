use dmg_core::{
    coverage::{Coverage, CoverageGap, CoverageGapKind, CoverageStatus, DiagnosticScope},
    digest::ContentDigest,
    migration::{
        AppliedStatus, ExecutionMode, FileDigest, IntegrityBasis, IntegrityChange,
        IntegrityComparison, IntegrityStatus, MigrationAdapter, MigrationDirection, MigrationFile,
        MigrationIdentity, MigrationPlan, StatementDelivery, TransactionWrapper,
    },
    policy::{DecisionReason, DecisionReasonKind, DecisionStatus, PolicyDecision, PolicyV1},
    report::{ReportMetadata, ReportV1},
    risk::{Confidence, Finding, RuleId, Severity},
    source::{ByteRange, RepoPath, SourceError, SourceMap},
    target::PostgresTarget,
};
use serde_json::{Value, json};

fn repo_path(value: &str) -> RepoPath {
    RepoPath::new(value).unwrap()
}

#[test]
fn preserves_original_utf8_and_lf_or_crlf_coordinates() {
    let input = concat!(
        "-- no-transaction\n",
        "ALTER TABLE app.accounts ADD COLUMN note text DEFAULT $m$é;🧪$m$; ALTER TABLE app.accounts\n",
        "  ADD COLUMN label text,\n",
        "  DROP COLUMN legacy_code;\n",
    );
    let path = repo_path("migrations/002_spans.up.sql");
    let crlf = input.replace('\n', "\r\n");
    for (source, second_statement, drop_start, drop_end) in
        [(input, 87, 139, 162), (&crlf, 88, 142, 165)]
    {
        let map = SourceMap::new(path.clone(), source);
        let position = map.position(second_statement).unwrap();
        assert_eq!((position.line.get(), position.column.get()), (2, 66));

        let range = ByteRange::new(drop_start, drop_end).unwrap();
        let span = map.span(range).unwrap();
        assert_eq!(span.path, path);
        assert_eq!(&source[span.bytes.as_range()], "DROP COLUMN legacy_code");
        assert_eq!((span.start.line.get(), span.start.column.get()), (4, 3));

        let eof = map.position(source.len()).unwrap();
        assert_eq!((eof.line.get(), eof.column.get()), (5, 1));
        let invalid_utf8_boundary = source.find('🧪').unwrap() + 1;
        assert_eq!(
            map.position(invalid_utf8_boundary),
            Err(SourceError::InvalidUtf8Boundary {
                offset: invalid_utf8_boundary
            })
        );
        assert!(matches!(
            map.position(source.len() + 1),
            Err(SourceError::OffsetOutOfBounds { .. })
        ));
    }

    let map = SourceMap::new(path, "é\r\nnext");
    assert_eq!(map.position(2).unwrap(), map.position(3).unwrap());
    assert_eq!(map.position(4).unwrap().line.get(), 2);
}

#[test]
fn rejects_unportable_source_paths_and_reversed_ranges() {
    for path in [
        "",
        "/x.sql",
        "../x.sql",
        "a//x.sql",
        "a/./x.sql",
        "a/../x.sql",
        "C:x.sql",
        "a\\x.sql",
    ] {
        assert!(RepoPath::new(path).is_err(), "accepted path {path:?}");
        assert!(serde_json::from_value::<RepoPath>(json!(path)).is_err());
    }
    assert_eq!(
        serde_json::to_value(repo_path("migrations/café.sql")).unwrap(),
        "migrations/café.sql"
    );
    assert!(ByteRange::new(10, 9).is_err());
    assert!(serde_json::from_value::<ByteRange>(json!({"start": 10, "end": 9})).is_err());
}

#[test]
fn computes_and_validates_original_byte_sha256() {
    let digest = ContentDigest::of(b"abc");
    let expected = "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad";
    assert_eq!(digest.to_string(), expected);
    assert_eq!(expected.parse::<ContentDigest>().unwrap(), digest);
    assert!(
        expected
            .to_ascii_uppercase()
            .parse::<ContentDigest>()
            .is_err()
    );
    assert!(
        format!("g{}", &expected[1..])
            .parse::<ContentDigest>()
            .is_err()
    );
    assert_ne!(ContentDigest::of(b"abc\n"), ContentDigest::of(b"abc\r\n"));
}

#[test]
fn native_identities_preserve_ordering_and_wire_types() {
    let flyway = |value| MigrationIdentity::flyway_version(value).unwrap();
    let repeatable = |value| MigrationIdentity::flyway_repeatable(value).unwrap();
    let supabase = |value| MigrationIdentity::supabase_version(value).unwrap();

    assert_eq!(flyway("01.02_0"), flyway("1.2"));
    assert!(flyway("1.2") < flyway("1.2.0.1"));
    assert!(flyway("1.2.0.1") < flyway("1.10"));
    assert!(flyway("1.10") < flyway("100000000000000000000000000000"));
    assert!(flyway("100000000000000000000000000000") < repeatable("first"));
    assert_eq!(repeatable("table_views"), repeatable("table views"));
    assert!(repeatable("😀") < repeatable("ﬀ"));
    assert!(repeatable("table_views").is_repeatable());
    assert!(supabase("01") < supabase("1"));
    assert!(supabase("10") < supabase("2"));

    for (identity, adapter, kind, value) in [
        (flyway("01.02_0"), "flyway", "flyway", "1.2"),
        (
            repeatable("table_views"),
            "flyway",
            "repeatable",
            "table views",
        ),
        (supabase("01"), "supabase", "supabase", "01"),
    ] {
        let json = serde_json::to_value(&identity).unwrap();
        assert_eq!(json["adapter"], adapter);
        assert_eq!(json["version"]["kind"], kind);
        assert_eq!(json["version"]["value"], value);
        assert_eq!(
            serde_json::from_value::<MigrationIdentity>(json).unwrap(),
            identity
        );
    }

    for (adapter, kind, value) in [
        ("plain", "supabase", "01"),
        ("supabase", "supabase", "1_2"),
        ("flyway", "flyway", "1..2"),
        ("flyway", "flyway", "9223372036854775807"),
    ] {
        let json =
            serde_json::json!({"adapter": adapter, "version": {"kind": kind, "value": value}});
        assert!(serde_json::from_value::<MigrationIdentity>(json).is_err());
    }
}

#[test]
fn distinguishes_git_provenance_and_sqlx_execution() {
    let identity = MigrationIdentity {
        adapter: MigrationAdapter::Sqlx,
        version: 2.into(),
    };
    let digest = ContentDigest::of(b"SELECT 1;\n");
    let before = repo_path("migrations/002_old.up.sql");
    let after = repo_path("migrations/002_new.up.sql");
    let plan = MigrationPlan {
        inventory: vec![MigrationFile {
            identity: identity.clone(),
            path: after.clone(),
            direction: MigrationDirection::Forward,
            digest,
            source_segments: vec![ByteRange::new(0, 10).unwrap()],
            execution: ExecutionMode::SQLX_NO_TRANSACTION,
        }],
        selected_forwards: Some(vec![after.clone()]),
        integrity: Some(IntegrityComparison {
            basis: IntegrityBasis::Git {
                base_commit: "1".repeat(40),
                head_commit: "2".repeat(40),
            },
            changes: vec![IntegrityChange {
                identity,
                direction: MigrationDirection::Forward,
                status: IntegrityStatus::Unchanged,
                before: Some(FileDigest {
                    path: before,
                    digest,
                }),
                after: Some(FileDigest {
                    path: after,
                    digest,
                }),
            }],
        }),
        applied_history: None,
        coverage: Coverage::default(),
    };

    let encoded = serde_json::to_value(&plan).unwrap();
    assert_eq!(encoded["integrity"]["basis"]["kind"], "git");
    assert_eq!(encoded["inventory"][0]["identity"]["version"], 2);
    assert_ne!(
        encoded["integrity"]["changes"][0]["before"]["path"],
        encoded["integrity"]["changes"][0]["after"]["path"]
    );
    assert_eq!(encoded["applied_history"], Value::Null);
    assert_eq!(AppliedStatus::default(), AppliedStatus::Unknown);
    assert_eq!(
        serde_json::from_value::<MigrationPlan>(encoded).unwrap(),
        plan
    );
    assert_eq!(
        plan.inventory[0].execution.transaction_wrapper,
        TransactionWrapper::None
    );
    assert_eq!(
        plan.inventory[0].execution.statement_delivery,
        StatementDelivery::SingleQuery
    );
    assert_eq!(
        ExecutionMode::PSQL_AUTOCOMMIT.statement_delivery,
        StatementDelivery::PerStatement
    );
}

#[test]
fn decodes_versioned_policy_and_rejects_unknown_configuration() {
    let minimal = PolicyV1::from_toml("schema_version = 1").unwrap();
    assert_eq!(minimal, PolicyV1::default());
    assert_eq!(minimal.deny_at, Severity::High);

    let configured = PolicyV1::from_toml(
        r#"
        schema_version = 1
        deny_at = "critical"

        [integrity]
        require_git_base = true

        [rules.PG005]
        severity = "medium"
    "#,
    )
    .unwrap();
    assert_eq!(configured.deny_at, Severity::Critical);
    assert!(configured.integrity.require_git_base);
    assert!(configured.integrity.deny_missing);
    assert_eq!(
        configured.rules[&"PG005".parse().unwrap()].severity,
        Severity::Medium
    );
    assert_eq!(
        PolicyV1::from_toml(&toml::to_string(&configured).unwrap()).unwrap(),
        configured
    );

    for invalid in [
        "deny_at = 'high'",
        "schema_version = 2",
        "schema_version = 1\ndeny_at = 'block'",
        "schema_version = 1\nrequire_coverage = false",
        "schema_version = 1\n[integrity]\nextra = true",
        "schema_version = 1\n[rules.PG000]\nseverity = 'low'",
        "schema_version = 1\n[rules.PG001]\nseverity = 'low'\nignore = true",
    ] {
        assert!(
            PolicyV1::from_toml(invalid).is_err(),
            "accepted {invalid:?}"
        );
    }
}

fn finding(start: usize, end: usize, severity: Severity, rule_id: RuleId) -> Finding {
    let span = SourceMap::new(repo_path("migrations/1_example.sql"), "SELECT 1;")
        .span(ByteRange::new(start, end).unwrap())
        .unwrap();
    Finding {
        span,
        rule_id,
        migration: MigrationIdentity {
            adapter: MigrationAdapter::Plain,
            version: 1.into(),
        },
        object: None,
        fingerprint: ContentDigest::of(rule_id.to_string().as_bytes()),
        severity,
        confidence: Confidence::High,
        explanation: "Fixture finding".into(),
        impact: "Fixture impact".into(),
        mitigation: "Fixture mitigation".into(),
        documentation: "https://www.postgresql.org/docs/18/".into(),
        evidence: Vec::new(),
    }
}

#[test]
fn emits_deterministic_report_and_reads_additive_fields() {
    let gap = CoverageGap {
        scope: DiagnosticScope::Repository,
        kind: CoverageGapKind::MissingIntegrityEvidence,
        message: "Required Git base is unavailable".into(),
        related_paths: vec![],
    };
    let plan = MigrationPlan {
        inventory: vec![],
        selected_forwards: None,
        integrity: None,
        applied_history: None,
        coverage: Coverage::from_gaps(vec![gap.clone()]),
    };
    let metadata = ReportMetadata {
        tool_version: "0.1.0".into(),
        ruleset_version: "0.1.0".into(),
        target: PostgresTarget::V17,
    };
    let findings = vec![
        finding(7, 8, Severity::High, "PG002".parse().unwrap()),
        finding(0, 6, Severity::Critical, "PG001".parse().unwrap()),
    ];
    let decision = PolicyDecision {
        status: DecisionStatus::Incomplete,
        policy_digest: ContentDigest::of(b"schema_version = 1"),
        reasons: vec![DecisionReason {
            kind: DecisionReasonKind::RequiredEvidence,
            message: gap.message.clone(),
            finding_fingerprint: None,
        }],
    };
    let report = ReportV1::new(
        metadata.clone(),
        plan.clone(),
        [gap.clone()],
        findings.clone(),
        Some(decision.clone()),
    );
    let reordered = ReportV1::new(
        metadata,
        plan,
        [gap],
        findings.into_iter().rev().collect(),
        Some(decision),
    );

    let encoded = serde_json::to_string(&report).unwrap();
    assert_eq!(encoded, serde_json::to_string(&reordered).unwrap());
    assert_eq!(report.coverage().status(), CoverageStatus::Incomplete);
    assert_eq!(report.coverage().gaps().len(), 1);
    assert_eq!(report.severity_summary().high, 1);
    assert_eq!(report.severity_summary().critical, 1);
    assert_eq!(report.findings()[0].span.bytes.start(), 0);

    let mut payload: Value = serde_json::from_str(&encoded).unwrap();
    assert_eq!(payload["schema_version"], 1);
    assert_eq!(payload["target"], 17);
    assert_eq!(payload["decision"]["status"], "incomplete");
    payload["new_report_field"] = json!({"future_field": true});
    assert_eq!(ReportV1::from_json(&payload.to_string()).unwrap(), report);

    payload["schema_version"] = json!(2);
    assert!(ReportV1::from_json(&payload.to_string()).is_err());
    for major in 15..=18 {
        let target = PostgresTarget::try_from(major).unwrap();
        assert_eq!(target.major(), major);
        assert_eq!(serde_json::to_value(target).unwrap(), json!(major));
    }
    assert!(PostgresTarget::try_from(14).is_err());
    assert!(PostgresTarget::try_from(19).is_err());
}
