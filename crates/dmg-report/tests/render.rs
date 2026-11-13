use dmg_core::{
    coverage::{Coverage, CoverageGap, CoverageGapKind, DiagnosticScope},
    digest::ContentDigest,
    inspection::TargetId,
    migration::{
        IntegrityBasis, IntegrityComparison, MigrationAdapter, MigrationIdentity, MigrationPlan,
    },
    policy::{
        ApprovalAuditV1, BaselineMatchV1, DecisionReason, DecisionReasonKind, DecisionStatus,
        PolicyAuditV1, PolicyDecision, RehearsalAuditV1,
    },
    rehearsal::RehearsalOutcome,
    report::{ReportMetadata, ReportV1},
    risk::{
        Confidence, DatabaseObject, EvidenceKind, Finding, FindingEvidence, ObjectKind,
        RelationLock, RelationLockObservation, Severity,
    },
    source::{ByteRange, RepoPath, SourceMap, SourceSpan},
    target::PostgresTarget,
};
use dmg_report::{
    write_github_annotations, write_gitlab_code_quality, write_html, write_markdown, write_sarif,
};
use serde_json::{Value, json};

fn path(value: &str) -> RepoPath {
    RepoPath::new(value).unwrap()
}

fn identity(version: i64) -> MigrationIdentity {
    MigrationIdentity {
        adapter: MigrationAdapter::Plain,
        version: version.into(),
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

#[test]
fn html_renders_text_only_and_preserves_gate_and_evidence() {
    let special = "<aside> & ' \" \u{202e}\u{1b}";
    let location = span("migrations/a<&b.sql", "🧪", 0, 4);
    let mut item = finding(location.clone(), Severity::High, special);
    item.object = Some(object(special));
    item.impact = special.into();
    item.mitigation = special.into();
    item.evidence[0].detail = special.into();
    let mut coverage = gap(
        DiagnosticScope::Path {
            path: location.path.clone(),
        },
        special,
    );
    coverage.related_paths = vec![location.path.clone()];
    let decision = PolicyDecision {
        status: DecisionStatus::Incomplete,
        policy_digest: ContentDigest::of(b"policy"),
        reasons: vec![DecisionReason {
            kind: DecisionReasonKind::RequiredEvidence,
            message: special.into(),
            finding_fingerprint: None,
        }],
    };
    let report = report(vec![item], vec![coverage], Some(decision))
        .with_policy_audit(Some(policy_audit()))
        .with_relation_locks(vec![RelationLockObservation {
            migration: identity(2),
            span: location,
            object: object(special),
            mode: RelationLock::Share,
        }]);
    let output = rendered_html(&report);
    assert!(output.contains("Content-Security-Policy"));
    assert!(output.contains("default-src 'none'"));
    assert!(output.contains("base-uri 'none'"));
    assert!(!output.contains("<aside>"));
    assert!(!output.contains("untrusted.invalid"));
    assert!(!output.contains(['\u{202e}', '\u{1b}']));
    assert!(output.contains("&lt;aside&gt; &amp; &#39; &quot;"));
    assert!(output.contains("migrations/a&lt;&amp;b.sql:1:1"));
    assert!(output.contains("<strong>Incomplete</strong> · Incomplete coverage"));
    assert!(output.contains("Required evidence"));
    assert!(output.contains("Predicted relation locks"));
    assert!(output.contains("Policy evidence"));
    assert!(output.contains("Rehearsal binding SHA-256"));
    assert!(output.contains("approver-a"));
    assert_eq!(output, rendered_html(&report));
    let md = markdown(&report);
    assert!(md.contains("Rehearsal **Succeeded**"));
    assert!(md.contains("Baseline match **PG004**"));
    assert!(md.contains("Approved **PG005**"));
}

#[test]
fn html_limits_findings_and_prefers_higher_severity() {
    let location = span("migrations/002_index.sql", "a", 0, 1);
    let detail = format!("{}after limit", "é".repeat(400));
    let mut findings = vec![finding(location.clone(), Severity::Low, &detail); 201];
    findings.push(finding(
        location,
        Severity::Critical,
        "Retain the critical finding.",
    ));
    let report = report(findings, vec![], None);
    let output = rendered_html(&report);
    assert_eq!(output.matches("<details>").count(), 200);
    assert!(output.contains("2 more findings in the Guard JSON report."));
    assert!(output.contains("Retain the critical finding."));
    assert!(output.contains('…'));
    assert!(!output.contains("after limit"));
    assert!(output.len() < 500 * 1024);
}

#[test]
fn sarif_preserves_unicode_columns_rule_indexes_fingerprints_and_coverage() {
    let path = "migrations/café #100%?🍞.sql";
    let location = span(path, "é🍞A\r\nB\r\n", 6, 9);
    let mut first = finding(
        location.clone(),
        Severity::Critical,
        "Name {0} [literal](/path).",
    );
    first.rule_id = "PG001".parse().unwrap();
    let second = finding(span(path, "é🍞A\r\nB\r\n", 9, 10), Severity::Low, "Review.");
    let mut coverage = vec![gap(
        DiagnosticScope::Span { span: location },
        "Unicode location.",
    )];
    for version in 0..52 {
        coverage.push(gap(
            DiagnosticScope::Migration {
                identity: identity(version),
            },
            "Review migration.",
        ));
    }
    let report =
        report(vec![second, first], coverage, None).with_policy_audit(Some(policy_audit()));
    let mut output = Vec::new();
    write_sarif(&report, &mut output).unwrap();
    let mut repeat = Vec::new();
    write_sarif(&report, &mut repeat).unwrap();
    assert_eq!(output, repeat);
    let sarif: Value = serde_json::from_slice(&output).unwrap();
    let run = &sarif["runs"][0];
    assert_eq!(run["columnKind"], "unicodeCodePoints");
    assert_eq!(run["newlineSequences"], json!(["\r\n", "\n"]));
    assert_eq!(
        run["originalUriBaseIds"]["%SRCROOT%"],
        json!({ "description": { "text": "Repository root" } })
    );
    assert_eq!(
        run["tool"]["driver"]["rules"],
        json!([{ "id": "PG001" }, { "id": "PG005" }])
    );
    let results = run["results"].as_array().unwrap();
    assert_eq!(results.len(), 2);
    let first = &results[0];
    assert_eq!(first["ruleIndex"], 0);
    assert_eq!(results[1]["ruleIndex"], 1);
    assert_eq!(first["level"], "error");
    assert_eq!(results[1]["level"], "note");
    assert_eq!(first["properties"]["dmgSeverity"], "critical");
    assert_eq!(
        first["fingerprints"]["dmgFinding/v1"],
        ContentDigest::of(b"risk").to_string()
    );
    assert_eq!(first["message"]["text"], r"Name {{0}} \[literal\](/path).");
    assert_eq!(
        first["locations"][0]["physicalLocation"],
        json!({
            "artifactLocation": {
                "uri": "migrations/caf%C3%A9%20%23100%25%3F%F0%9F%8D%9E.sql",
                "uriBaseId": "%SRCROOT%"
            },
            "region": { "startLine": 1, "startColumn": 3, "endLine": 2, "endColumn": 1 }
        })
    );
    assert_eq!(run["properties"]["dmgCoverage"], "incomplete");
    assert_eq!(run["properties"]["dmgCoverageGapCount"], 53);
    assert_eq!(run["properties"]["dmgRehearsal"]["outcome"], "succeeded");
    assert_eq!(run["invocations"][0]["executionSuccessful"], false);
    let notifications = run["invocations"][0]["toolExecutionNotifications"]
        .as_array()
        .unwrap();
    assert_eq!(notifications.len(), 52);
    assert_eq!(notifications[0]["level"], "warning");
    assert!(
        notifications[51]["message"]["text"]
            .as_str()
            .unwrap()
            .contains("3 more coverage gaps")
    );
    assert!(!String::from_utf8_lossy(&output).contains("untrusted.invalid"));
}

#[test]
fn gitlab_outputs_original_paths_all_severities_and_no_preamble() {
    let name = "migrations/café %23.sql";
    let severities = [
        (Severity::Info, "info"),
        (Severity::Low, "minor"),
        (Severity::Medium, "major"),
        (Severity::High, "critical"),
        (Severity::Critical, "blocker"),
    ];
    let findings = severities
        .iter()
        .enumerate()
        .map(|(index, &(severity, _))| {
            let mut result = finding(
                span(name, "a\n🧪", 2, 6),
                severity,
                "Review \r\n this finding \u{2066}",
            );
            result.fingerprint = ContentDigest::of(&[index as u8]);
            result
        })
        .collect();
    let report = report(
        findings,
        vec![gap(DiagnosticScope::Repository, "Review coverage.")],
        None,
    );
    let mut output = Vec::new();
    write_gitlab_code_quality(&report, &mut output).unwrap();
    assert_eq!(output[0], b'[');
    let array: Vec<Value> = serde_json::from_slice(&output).unwrap();
    assert_eq!(array.len(), severities.len());
    for (issue, finding) in array.iter().zip(report.findings()) {
        let severity = severities
            .iter()
            .find(|(level, _)| *level == finding.severity)
            .unwrap()
            .1;
        assert_eq!(
            issue,
            &json!({
                "check_name": "PG005",
                "description": "Review    this finding  ",
            "fingerprint": finding.fingerprint,
                "severity": severity,
                "location": { "path": name, "lines": { "begin": 2 } }
            })
        );
    }
}

fn rendered_html(report: &ReportV1) -> String {
    let mut output = Vec::new();
    write_html(report, &mut output).unwrap();
    String::from_utf8(output).unwrap()
}

fn policy_audit() -> PolicyAuditV1 {
    PolicyAuditV1 {
        evidence_sha256: ContentDigest::of(b"evidence"),
        repository_id: TargetId::new("repo-a").unwrap(),
        target_id: TargetId::new("target-a").unwrap(),
        baseline_reference_commit: Some("a".repeat(40)),
        baseline_matches: vec![BaselineMatchV1 {
            finding_fingerprint: ContentDigest::of(b"risk"),
            rule_id: "PG004".parse().unwrap(),
        }],
        approvals: vec![ApprovalAuditV1 {
            finding_fingerprint: ContentDigest::of(b"risk"),
            rule_id: "PG005".parse().unwrap(),
            approver: TargetId::new("approver-a").unwrap(),
            justification_sha256: ContentDigest::of(b"justification"),
            issued_at_unix_seconds: 1,
            expires_at_unix_seconds: 100,
            rehearsal_report_sha256: Some(ContentDigest::of(b"rehearsal")),
        }],
        rehearsal: Some(RehearsalAuditV1 {
            report_sha256: ContentDigest::of(b"rehearsal"),
            binding_sha256: ContentDigest::of(b"binding"),
            completed_at_unix_seconds: 50,
            outcome: RehearsalOutcome::Succeeded,
        }),
    }
}
