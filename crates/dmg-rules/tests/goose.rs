use std::{
    fs, io,
    path::PathBuf,
    sync::atomic::{AtomicUsize, Ordering},
};

use dmg_core::{
    coverage::{CoverageGapKind, CoverageStatus},
    source::RepoPath,
    target::PostgresTarget,
};
use dmg_discovery::{Adapter, DiscoveryLimits, DiscoveryOptions, discover_worktree};
use dmg_postgres::parse_plan;
use dmg_rules::{Analysis, Rule, RuleOptions, analyze};

fn analyze_goose(candidate: &str, target: PostgresTarget) -> Analysis {
    let root = TestRepo::new().unwrap();
    let migrations = root.0.join("migrations");
    fs::create_dir(&migrations).unwrap();
    fs::write(
        migrations.join("1_base.sql"),
        "-- +goose Up\nCREATE TABLE public.probe (id integer PRIMARY KEY);\n",
    )
    .unwrap();
    fs::write(migrations.join("2_candidate.sql"), candidate).unwrap();

    let options = DiscoveryOptions::new(
        vec![RepoPath::new("migrations").unwrap()],
        Adapter::Goose,
        DiscoveryLimits::default(),
    )
    .unwrap();
    let discovery = discover_worktree(&root.0, &options);
    assert_eq!(discovery.plan.coverage.status(), CoverageStatus::Complete);
    let parsed = parse_plan(&discovery.plan, target, |path| discovery.source_bytes(path));
    analyze(&parsed, target, &RuleOptions::default())
}

#[test]
fn goose_query_groups_determine_concurrent_index_transaction_risk() {
    let alter = "ALTER TABLE public.probe ADD COLUMN note text;";
    let index = "CREATE INDEX CONCURRENTLY probe_note ON public.probe(note);";
    let cases = [
        (format!("-- +goose Up\n{alter}\n{index}\n"), true),
        (
            format!("-- +goose NO TRANSACTION\n-- +goose Up\n{alter}\n{index}\n"),
            false,
        ),
        (
            format!("-- +goose NO TRANSACTION\n-- +goose Up\n{alter} {index}\n"),
            true,
        ),
        (
            format!(
                "-- +goose NO TRANSACTION\n-- +goose Up\n-- +goose StatementBegin\n{alter}\n{index}\n-- +goose StatementEnd\n"
            ),
            true,
        ),
        (
            format!("-- +goose NO TRANSACTION\n-- +goose Up\n{alter}-- inline comment\n{index}\n"),
            true,
        ),
        (
            format!("-- +goose NO TRANSACTION\n-- +goose Up\n{alter} -- inline comment\n{index}\n"),
            false,
        ),
        (
            format!("-- +goose Up\n{alter}\n{index}\n-- +goose Down\n-- +goose NO TRANSACTION\n"),
            false,
        ),
    ];
    for target in [PostgresTarget::V17, PostgresTarget::V18] {
        for (candidate, conflict) in &cases {
            let analysis = analyze_goose(candidate, target);
            assert_eq!(
                analysis.coverage.status(),
                CoverageStatus::Complete,
                "{candidate}"
            );
            let finding = analysis
                .findings
                .iter()
                .find(|finding| finding.rule_id == Rule::Pg007.id());
            assert_eq!(finding.is_some(), *conflict, "{candidate}");
            if let Some(finding) = finding {
                let start = candidate.find(index).unwrap();
                let end = start + index.len() - 1;
                assert_eq!(finding.span.bytes.as_range(), start..end);
                assert_eq!(finding.span.path.as_str(), "migrations/2_candidate.sql");
            }
        }
    }
}

#[test]
fn goose_preserves_earlier_findings_with_uncertain_authored_transaction() {
    let candidate = "-- +goose NO TRANSACTION\r\n-- +goose Up\r\n\
                     TRUNCATE public.probe;\r\nBEGIN;\r\n\
                     CREATE INDEX CONCURRENTLY probe_id ON public.probe(id);\r\n";
    let analysis = analyze_goose(candidate, PostgresTarget::V18);
    let gaps = analysis.coverage.gaps();
    assert_eq!(
        gaps.iter()
            .filter(|gap| gap.kind == CoverageGapKind::UnresolvedTransactionMode)
            .count(),
        2
    );
    assert!(
        analysis
            .findings
            .iter()
            .any(|finding| finding.rule_id == Rule::Pg001.id())
    );
    assert!(
        !analysis
            .findings
            .iter()
            .any(|finding| finding.rule_id == Rule::Pg007.id())
    );
}

#[test]
fn goose_marks_crlf_inside_sql_tokens_incomplete() {
    let candidate = "-- +goose Up\r\n-- +goose StatementBegin\r\n\
                     ALTER TABLE public.probe ADD COLUMN \"é\r\n🧪\" integer;\r\n\
                     -- +goose StatementEnd\r\n";
    let analysis = analyze_goose(candidate, PostgresTarget::V17);
    let gaps = analysis.coverage.gaps();
    assert_eq!(gaps.len(), 1);
    assert_eq!(gaps[0].kind, CoverageGapKind::UnsupportedInput);
    assert!(gaps[0].message.contains("normalizes CRLF"));
    let dmg_core::coverage::DiagnosticScope::Span { span } = &gaps[0].scope else {
        panic!("expected original token range");
    };
    assert_eq!(&candidate[span.bytes.as_range()], "\"é\r\n🧪\"");
}

static SEQUENCE: AtomicUsize = AtomicUsize::new(0);

struct TestRepo(PathBuf);

impl TestRepo {
    fn new() -> io::Result<Self> {
        loop {
            let sequence = SEQUENCE.fetch_add(1, Ordering::Relaxed);
            let root = std::env::temp_dir()
                .join(format!("dmg-goose-rules-{}-{sequence}", std::process::id()));
            match fs::create_dir(&root) {
                Ok(()) => return Ok(Self(root)),
                Err(error) if error.kind() == io::ErrorKind::AlreadyExists => continue,
                Err(error) => return Err(error),
            }
        }
    }
}

impl Drop for TestRepo {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}
