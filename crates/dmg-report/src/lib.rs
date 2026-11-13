//! Bounded pull request output from a versioned Guard report.
#![forbid(unsafe_code)]

mod github;
mod markdown;

use dmg_core::{
    coverage::CoverageGapKind,
    risk::{Finding, Severity},
};

pub use github::write_github_annotations;
pub use markdown::write_markdown;

fn prioritized(findings: &[Finding]) -> impl Iterator<Item = &Finding> {
    [
        Severity::Critical,
        Severity::High,
        Severity::Medium,
        Severity::Low,
        Severity::Info,
    ]
    .into_iter()
    .flat_map(move |severity| {
        findings
            .iter()
            .filter(move |finding| finding.severity == severity)
    })
}

fn severity_name(severity: Severity) -> &'static str {
    match severity {
        Severity::Critical => "CRITICAL",
        Severity::High => "HIGH",
        Severity::Medium => "MEDIUM",
        Severity::Low => "LOW",
        Severity::Info => "INFO",
    }
}

fn gap_name(kind: CoverageGapKind) -> &'static str {
    match kind {
        CoverageGapKind::InvalidPath => "Invalid path",
        CoverageGapKind::UnreadableInput => "Unreadable input",
        CoverageGapKind::UnsupportedInput => "Unsupported input",
        CoverageGapKind::InputLimitExceeded => "Input limit exceeded",
        CoverageGapKind::AmbiguousIdentity => "Ambiguous migration identity",
        CoverageGapKind::UnpairedMigration => "Unpaired migration",
        CoverageGapKind::MissingIntegrityEvidence => "Missing integrity evidence",
        CoverageGapKind::MissingAppliedHistory => "Missing applied history",
        CoverageGapKind::ParseError => "SQL parse error",
        CoverageGapKind::UnsupportedSql => "Unsupported SQL",
        CoverageGapKind::IncompatibleTarget => "Incompatible PostgreSQL target",
        CoverageGapKind::UnresolvedDependency => "Unresolved dependency",
        CoverageGapKind::UnresolvedTransactionMode => "Unresolved transaction mode",
    }
}

fn is_control(character: char) -> bool {
    character.is_control()
        || matches!(
            character,
            '\u{061c}' | '\u{200e}' | '\u{200f}' | '\u{2028}'..='\u{202e}'
                | '\u{2066}'..='\u{2069}' | '\u{feff}'
        )
}

fn bounded_line(value: &str, max_bytes: usize) -> String {
    let prefix = &value[..value.floor_char_boundary(max_bytes)];
    let mut line: String = prefix
        .chars()
        .map(|character| {
            if is_control(character) {
                ' '
            } else {
                character
            }
        })
        .collect();
    if prefix.len() < value.len() {
        line.truncate(line.trim_end().len());
        line.push('…');
    }
    line
}
