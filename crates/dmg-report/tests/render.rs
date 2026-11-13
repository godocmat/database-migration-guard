use dmg_core::{
    coverage::{Coverage, CoverageGap, CoverageGapKind, DiagnosticScope},
    digest::ContentDigest,
    migration::{
        IntegrityBasis, IntegrityComparison, MigrationAdapter, MigrationIdentity, MigrationPlan,
    },
    policy::{DecisionReason, DecisionReasonKind, DecisionStatus, PolicyDecision},
    report::{ReportMetadata, ReportV1},
    risk::{
        Confidence, DatabaseObject, EvidenceKind, Finding, FindingEvidence, ObjectKind,
        RelationLock, RelationLockObservation, Severity,
    },
    source::{ByteRange, RepoPath, SourceMap, SourceSpan},
    target::PostgresTarget,
};
use dmg_report::{write_github_annotations, write_markdown};

fn path(value: &str) -> RepoPath {
    RepoPath::new(value).unwrap()
}

fn identity(version: i64) -> MigrationIdentity {
    MigrationIdentity {
        adapter: MigrationAdapter::Plain,
        version,
    }
}

fn object(name: &str) -> DatabaseObject {
    DatabaseObject {
        kind: ObjectKind::Relation,
        schema: Some("public".into()),
        name: name.into(),
        member: None,
    }
}

fn span(file: &str, text: &str, start: usize, end: usize) -> SourceSpan {
    SourceMap::new(path(file), text)
        .span(ByteRange::new(start, end).unwrap())
        .unwrap()
}

fn finding(span: SourceSpan, severity: Severity, explanation: &str) -> Finding {
    Finding {
        span,
        rule_id: "PG005".parse().unwrap(),
        migration: identity(2),
        object: Some(object("ledger")),
        fingerprint: ContentDigest::of(b"risk"),
        severity,
        confidence: Confidence::High,
        explanation: explanation.into(),
        impact: "Writers can wait for this migration.".into(),
        mitigation: "Use a supported concurrent index build.".into(),
        documentation: "https://untrusted.invalid/from-report".into(),
        evidence: vec![FindingEvidence {
            kind: EvidenceKind::StaticPrediction,
            detail: "A regular index build requests a relation lock.".into(),
            artifact_digest: None,
        }],
    }
}

fn gap(scope: DiagnosticScope, message: &str) -> CoverageGap {
    CoverageGap {
        scope,
        kind: CoverageGapKind::UnsupportedSql,
        message: message.into(),
        related_paths: vec![],
    }
}

fn report(
    findings: Vec<Finding>,
    gaps: Vec<CoverageGap>,
    decision: Option<PolicyDecision>,
) -> ReportV1 {
    ReportV1::new(
        ReportMetadata {
            tool_version: "0.1.0".into(),
            ruleset_version: "1".into(),
            target: PostgresTarget::V18,
        },
        MigrationPlan {
            inventory: vec![],
            selected_forwards: Some(vec![path("migrations/002_index.sql")]),
            integrity: Some(IntegrityComparison {
                basis: IntegrityBasis::Git {
                    base_commit: "a".repeat(40),
                    head_commit: "b".repeat(40),
                },
                changes: vec![],
            }),
            applied_history: None,
            coverage: Coverage::Complete,
        },
        gaps,
        findings,
        decision,
    )
}

fn markdown(report: &ReportV1) -> String {
    let mut output = Vec::new();
    write_markdown(report, &mut output).unwrap();
    String::from_utf8(output).unwrap()
}

fn github(report: &ReportV1) -> String {
    let mut output = Vec::new();
    write_github_annotations(report, &mut output).unwrap();
    String::from_utf8(output).unwrap()
}

#[test]
fn markdown_shows_gate_inputs_and_actionable_finding_details() {
    let location = span("migrations/002_index.sql", "\r\n   index", 5, 10);
    let decision = PolicyDecision {
        status: DecisionStatus::Incomplete,
        policy_digest: ContentDigest::of(b"policy"),
        reasons: vec![DecisionReason {
            kind: DecisionReasonKind::RequiredCoverage,
            message: "A statement requires review.".into(),
            finding_fingerprint: None,
        }],
    };
    let report = report(
        vec![finding(
            location.clone(),
            Severity::High,
            "Index build can block writes.",
        )],
        vec![gap(
            DiagnosticScope::Span {
                span: location.clone(),
            },
            "The operation has limited coverage.",
        )],
        Some(decision),
    )
    .with_relation_locks(vec![RelationLockObservation {
        migration: identity(2),
        span: location,
        object: object("ledger"),
        mode: RelationLock::Share,
    }]);
    let output = markdown(&report);
    for fragment in [
        "Database Migration Guard · Incomplete",
        "PostgreSQL 18",
        "Git base",
        "Policy SHA-256",
        "**Required coverage** · A statement requires review.",
        "**Unsupported SQL** · `migrations/002_index.sql:2:4`",
        "Critical 0 · High 1 · Medium 0 · Low 0 · Info 0",
        "**HIGH PG005** at `migrations/002_index.sql:2:4` on `public.ledger`",
        "**Impact** · Writers can wait",
        "**Static prediction** · A regular index build",
        "**Mitigation** · Use a supported concurrent index build.",
        "**SHARE** on `public.ledger`",
    ] {
        assert!(output.contains(fragment), "{fragment}\n{output}");
    }
    assert!(!output.contains("untrusted.invalid"));
    assert_eq!(output, markdown(&report));
}

#[test]
fn markdown_treats_paths_and_untrusted_text_as_literal_content() {
    let location = span("migrations/café``[name].sql", "🧪", 0, 4);
    let mut finding = finding(
        location,
        Severity::Low,
        "Label [draft](/docs) <mark> <!-- plain --> & value\r\n\u{1b}continued",
    );
    finding.impact = "See https://example.invalid/path or mention @reviewer.".into();
    finding.mitigation = "www.example.invalid is reference text.".into();
    let output = markdown(&report(vec![finding], vec![], None));
    assert!(output.contains("```migrations/café``[name].sql:1:1```"));
    assert!(
        output.contains(r"Label \[draft\](/docs) \<mark\> \<!-- plain --\> \& value   continued")
    );
    assert!(output.contains("`See https://example.invalid/path or mention @reviewer.`"));
    assert!(output.contains("`www.example.invalid is reference text.`"));
    assert!(!output.contains(['\r', '\u{1b}']));
    assert!(output.contains("Database Migration Guard · Scan"));
}

#[test]
fn github_escapes_properties_and_preserves_original_positions() {
    let report = report(
        vec![finding(
            span("migrations/a,%0A.sql", "é🧪 abc", 7, 9),
            Severity::High,
            "First 25% complete\r\nSecond line\u{2028}continued",
        )],
        vec![gap(
            DiagnosticScope::Span {
                span: span("migrations/unknown.sql", "a\r\nb\r\nc", 0, 6),
            },
            "Check the next statement.",
        )],
        None,
    );
    let output = github(&report);
    assert_eq!(output.lines().count(), 2);
    assert!(output.contains("::warning title=Unsupported SQL,file=migrations/unknown.sql,line=1,endLine=2::Check the next statement."));
    assert!(output.contains("::error title=HIGH PG005,file=migrations/a%2C%250A.sql,line=1,col=4,endColumn=5::First 25%25 complete  Second line continued"));
}

#[test]
fn github_keeps_warning_if_source_location_cannot_be_represented() {
    let report = report(
        vec![],
        vec![gap(
            DiagnosticScope::Path {
                path: path("migrations/new\nline.sql"),
            },
            "Filename needs review.",
        )],
        None,
    );
    assert_eq!(
        github(&report),
        "::warning title=Unsupported SQL::Filename needs review.\n"
    );
}

#[test]
fn outputs_bound_rows_and_fields_and_keep_omitted_counts() {
    let long_text = format!("{}{}", "📚".repeat(300), "END OF ORIGINAL TEXT");
    let path = format!("migrations/{}.sql", "`".repeat(1200));
    let span = span(&path, "é", 0, 2);
    let mut findings = Vec::new();
    for severity in [
        Severity::Low,
        Severity::Medium,
        Severity::High,
        Severity::Critical,
    ] {
        for _ in 0..12 {
            let mut finding = finding(span.clone(), severity, &long_text);
            finding.impact = long_text.clone();
            finding.mitigation = long_text.clone();
            findings.push(finding);
        }
    }
    let gaps = (0..15)
        .map(|index| {
            gap(
                DiagnosticScope::Migration {
                    identity: identity(index),
                },
                &long_text,
            )
        })
        .collect();
    let report = report(findings, gaps, None);

    let summary = markdown(&report);
    assert!(summary.contains("18 more findings in the JSON report."));
    assert!(summary.contains("3 more coverage gaps in the JSON report."));
    assert_eq!(summary.matches("**CRITICAL PG005**").count(), 12);
    assert_eq!(summary.matches("**HIGH PG005**").count(), 12);
    assert_eq!(summary.matches("**MEDIUM PG005**").count(), 6);
    assert!(summary.len() < 250 * 1024);
    assert!(summary.contains('…'));
    assert!(!summary.contains("END OF ORIGINAL TEXT"));

    let annotations = github(&report);
    assert_eq!(
        annotations
            .lines()
            .filter(|line| line.starts_with("::error "))
            .count(),
        10
    );
    assert_eq!(
        annotations
            .lines()
            .filter(|line| line.starts_with("::warning "))
            .count(),
        10
    );
    assert_eq!(
        annotations
            .lines()
            .filter(|line| line.starts_with("::notice "))
            .count(),
        10
    );
    assert!(annotations.contains("Omitted 28 findings and 6 coverage gaps."));
    assert!(!annotations.contains("END OF ORIGINAL TEXT"));
    assert!(!annotations.contains(",file="));
}
