use std::{
    env, fs,
    path::{Path, PathBuf},
    process::Command,
    time::{SystemTime, UNIX_EPOCH},
};

use dmg_core::{
    coverage::{CoverageGap, CoverageGapKind, CoverageStatus, DiagnosticScope},
    migration::IntegrityStatus,
    risk::{DatabaseObject, Severity},
    source::{RepoPath, SourceSpan},
    target::PostgresTarget,
};
use dmg_discovery::{
    Adapter, Discovery, DiscoveryLimits, DiscoveryOptions, GitRevisions, PlainSql,
    PlainTransaction, SqlxLayout, VersionOrder, discover_git,
};
use dmg_postgres::{ParsedPlan, parse_plan};
use dmg_rules::{Analysis, RelationLock, RuleOptions, analyze};
use serde::Deserialize;

#[test]
#[ignore = "requires Python 3 and materializes the public development corpus"]
fn analyze_development_corpus() {
    let script = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/dmg-02/corpus.py");
    let python = env::var_os("DMG_FIXTURE_PYTHON")
        .unwrap_or_else(|| if cfg!(windows) { "python" } else { "python3" }.into());
    let listed = Command::new(&python)
        .arg("-B")
        .arg(&script)
        .arg("list")
        .output()
        .unwrap();
    assert!(
        listed.status.success(),
        "{}",
        String::from_utf8_lossy(&listed.stderr)
    );

    let ids = String::from_utf8(listed.stdout).unwrap();
    let selected_case = env::var("DMG_FIXTURE_CASE").ok();
    let mut count = 0;
    let mut failures = Vec::new();
    for id in ids.lines().filter(|id| {
        selected_case
            .as_ref()
            .is_none_or(|prefix| id.starts_with(prefix))
    }) {
        let fixture = TemporaryFixture::new(count);
        let output = Command::new(&python)
            .arg("-B")
            .arg(&script)
            .args(["materialize", id])
            .arg(&fixture.0)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{id}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        let metadata: Fixture =
            serde_json::from_slice(&fs::read(fixture.0.join("case.json")).unwrap()).unwrap();
        let revisions = GitRevisions::new(&metadata.git.base, &metadata.git.head).unwrap();
        let discovery = discover_git(&fixture.0.join("repo"), &revisions, &metadata.options());
        let plan = parse_plan(&discovery.plan, metadata.target_major, |path| {
            discovery.source_bytes(path)
        });
        let analysis = analyze(&plan, metadata.target_major, &RuleOptions::default());

        for error in metadata.verify(id, &discovery, &plan, &analysis) {
            failures.push(format!("{id}: {error}"));
        }
        count += 1;
    }
    if selected_case.is_none() {
        assert_eq!(count, 130);
    } else {
        assert!(count > 0, "no development fixtures matched");
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
    println!("Verified {count} public discovery, parser, and risk combinations");
}

#[derive(Deserialize)]
struct Fixture {
    migration_root: RepoPath,
    profile: String,
    target_major: PostgresTarget,
    git: RevisionPair,
    candidate: Vec<CandidateFile>,
    expected: Expected,
    source_spans: Vec<ExpectedSpan>,
    forward_statement_count: Option<usize>,
}

#[derive(Deserialize)]
struct RevisionPair {
    base: String,
    head: String,
}

#[derive(Deserialize)]
struct CandidateFile {
    mode: String,
}

#[derive(Deserialize)]
struct Expected {
    coverage: CoverageStatus,
    check_exit: u8,
    #[serde(default)]
    findings: Vec<ExpectedFinding>,
    #[serde(default)]
    absent: Vec<String>,
    #[serde(default)]
    diagnostics: Vec<ExpectedDiagnostic>,
}

#[derive(Deserialize)]
struct ExpectedFinding {
    rule: String,
    severity: String,
    path: RepoPath,
    anchor: String,
    object: String,
}

#[derive(Deserialize)]
struct ExpectedDiagnostic {
    kind: String,
    paths: Vec<RepoPath>,
    anchor: Option<String>,
}

#[derive(Deserialize)]
struct ExpectedSpan {
    kind: String,
    path: RepoPath,
    bytes: [usize; 2],
    line: usize,
    column: usize,
    text: String,
}

impl Fixture {
    fn options(&self) -> DiscoveryOptions {
        let adapter = match self.profile.as_str() {
            "plain" => {
                let transaction =
                    if self.candidate.iter().any(|file| {
                        matches!(file.mode.as_str(), "psql-autocommit" | "psql-explicit")
                    }) {
                        PlainTransaction::Autocommit
                    } else {
                        PlainTransaction::File
                    };
                Adapter::Plain(
                    PlainSql::new(
                        r"^([0-9]+)_[a-z][a-z0-9_]*\.sql$",
                        VersionOrder::PositiveInteger,
                        transaction,
                    )
                    .unwrap(),
                )
            }
            "sqlx-simple" => Adapter::Sqlx(SqlxLayout::Simple),
            "sqlx-reversible" => Adapter::Sqlx(SqlxLayout::Reversible),
            unexpected => panic!("unexpected fixture profile {unexpected}"),
        };
        DiscoveryOptions::new(
            vec![self.migration_root.clone()],
            adapter,
            DiscoveryLimits::default(),
        )
        .unwrap()
    }

    fn verify(
        &self,
        id: &str,
        discovery: &Discovery,
        plan: &ParsedPlan,
        analysis: &Analysis,
    ) -> Vec<String> {
        let mut failures = Vec::new();
        if analysis.coverage.status() != self.expected.coverage {
            failures.push(format!(
                "expected {:?} coverage, got {:?}",
                self.expected.coverage, analysis.coverage
            ));
        }

        // DMG-06 verifies the default gate inputs before the DMG-07 evaluator is wired.
        let integrity_denied = discovery.plan.integrity.as_ref().is_some_and(|comparison| {
            comparison.changes.iter().any(|change| {
                matches!(
                    change.status,
                    IntegrityStatus::Modified | IntegrityStatus::Missing
                )
            })
        });
        let risk_denied = analysis
            .findings
            .iter()
            .any(|finding| finding.severity >= Severity::High);
        let exit = if analysis.coverage.status() == CoverageStatus::Incomplete
            || discovery.plan.integrity.is_none()
        {
            3
        } else {
            u8::from(integrity_denied || risk_denied)
        };
        if exit != self.expected.check_exit {
            failures.push(format!(
                "expected gate inputs to yield {}, got {exit}",
                self.expected.check_exit
            ));
        }

        for expected in &self.expected.findings {
            if !analysis.findings.iter().any(|finding| {
                finding.rule_id.to_string() == expected.rule
                    && severity_name(finding.severity) == expected.severity
                    && finding.span.path == expected.path
                    && finding
                        .object
                        .as_ref()
                        .is_some_and(|object| object_name(object) == expected.object)
                    && contains_anchor(discovery, &finding.span, &expected.anchor)
            }) {
                failures.push(format!(
                    "missing {} ({}) at {} containing {:?} for {}",
                    expected.rule,
                    expected.severity,
                    expected.path,
                    expected.anchor,
                    expected.object
                ));
            }
        }
        for absent in &self.expected.absent {
            if analysis
                .findings
                .iter()
                .any(|finding| finding.rule_id.to_string() == *absent)
            {
                failures.push(format!("unexpected finding {absent}"));
            }
        }
        let expected_locks: &[(&str, RelationLock)] = match id.split('/').next().unwrap() {
            "R03a" | "R03b" | "R04a" | "R04b" => &[("app.accounts", RelationLock::AccessExclusive)],
            "R04c" => &[("app.accounts", RelationLock::ShareUpdateExclusive)],
            "R05" => &[("app.accounts", RelationLock::Share)],
            "V02" if self.target_major == PostgresTarget::V18 => &[
                ("app.accounts", RelationLock::ShareRowExclusive),
                ("app.events", RelationLock::ShareRowExclusive),
            ],
            _ => &[],
        };
        for (object, mode) in expected_locks {
            if !analysis
                .locks
                .iter()
                .any(|lock| lock.mode == *mode && object_name(&lock.object) == *object)
            {
                failures.push(format!("missing predicted {mode:?} lock for {object}"));
            }
        }
        for expected in &self.expected.diagnostics {
            let kind = match expected.kind.as_str() {
                "duplicate_forward" => CoverageGapKind::AmbiguousIdentity,
                "unpaired_reversible" => CoverageGapKind::UnpairedMigration,
                "missing_git_base" => CoverageGapKind::MissingIntegrityEvidence,
                "parse_error" => CoverageGapKind::ParseError,
                "unsupported_dynamic_sql" => CoverageGapKind::UnsupportedSql,
                "target_incompatible" => CoverageGapKind::IncompatibleTarget,
                other => panic!("unexpected fixture diagnostic {other}"),
            };
            let matching: Vec<_> = analysis
                .coverage
                .gaps()
                .iter()
                .filter(|gap| gap.kind == kind)
                .collect();
            let missing_path = expected
                .paths
                .iter()
                .any(|path| !matching.iter().any(|gap| covers_path(gap, path)));
            let missing_anchor = expected.anchor.as_ref().is_some_and(|anchor| !matching.iter().any(|gap| {
                matches!(&gap.scope, DiagnosticScope::Span { span } if expected.paths.contains(&span.path) && contains_anchor(discovery, span, anchor))
            }));
            if matching.is_empty() || missing_path || missing_anchor {
                failures.push(format!(
                    "missing diagnostic {} with its required source scope",
                    expected.kind
                ));
            }
        }
        if let Some(expected) = self.forward_statement_count {
            let selected = plan.selected_forwards.as_ref().unwrap();
            let actual = plan
                .migrations
                .iter()
                .filter(|migration| selected.contains(&migration.path))
                .map(|migration| migration.statements.len())
                .sum::<usize>();
            if actual != expected {
                failures.push(format!(
                    "expected {expected} selected forward statements, got {actual}"
                ));
            }
        }
        for expected in &self.source_spans {
            let original = discovery.source(&expected.path).unwrap();
            let valid_start = |span: &SourceSpan| {
                span.path == expected.path
                    && span.bytes.start() == expected.bytes[0]
                    && span.start.line.get() == expected.line
                    && span.start.column.get() == expected.column
                    && original.get(expected.bytes[0]..expected.bytes[1])
                        == Some(expected.text.as_str())
            };
            let matches = match expected.kind.as_str() {
                "second_statement_start" => plan
                    .migrations
                    .iter()
                    .find(|migration| migration.path == expected.path)
                    .and_then(|migration| migration.statements.get(1))
                    .is_some_and(|statement| {
                        valid_start(&statement.span)
                            && statement.span.bytes.end() >= expected.bytes[1]
                    }),
                "drop_subcommand" => analysis.findings.iter().any(|finding| {
                    finding.rule_id.to_string() == "PG001"
                        && valid_start(&finding.span)
                        && finding.span.bytes.end() == expected.bytes[1]
                }),
                unexpected => panic!("unexpected fixture source span {unexpected}"),
            };
            if !matches {
                failures.push(format!(
                    "missing exact {} span {:?} at {}",
                    expected.kind, expected.bytes, expected.path
                ));
            }
        }
        failures
    }
}

fn contains_anchor(discovery: &Discovery, span: &SourceSpan, anchor: &str) -> bool {
    discovery
        .source(&span.path)
        .and_then(|source| source.get(span.bytes.as_range()))
        .is_some_and(|source| source.replace("\r\n", "\n").contains(anchor))
}

fn covers_path(gap: &CoverageGap, path: &RepoPath) -> bool {
    gap.related_paths.contains(path)
        || match &gap.scope {
            DiagnosticScope::Path { path: actual } => path == actual,
            DiagnosticScope::Span { span } => path == &span.path,
            _ => false,
        }
}

fn severity_name(severity: Severity) -> &'static str {
    match severity {
        Severity::Info => "INFO",
        Severity::Low => "LOW",
        Severity::Medium => "MEDIUM",
        Severity::High => "HIGH",
        Severity::Critical => "CRITICAL",
    }
}

fn object_name(object: &DatabaseObject) -> String {
    object
        .schema
        .iter()
        .chain([&object.name])
        .chain(object.member.iter())
        .map(String::as_str)
        .collect::<Vec<_>>()
        .join(".")
}

struct TemporaryFixture(PathBuf);

impl TemporaryFixture {
    fn new(index: usize) -> Self {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        Self(env::temp_dir().join(format!(
            "dmg06-corpus-{}-{index}-{unique}",
            std::process::id()
        )))
    }
}

impl Drop for TemporaryFixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}
