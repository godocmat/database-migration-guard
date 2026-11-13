//! Review output from a versioned Guard report.
#![forbid(unsafe_code)]

mod diff;
mod evidence;
mod github;
mod gitlab;
mod html;
mod machine;
mod markdown;
mod rehearsal;
mod sarif;

use std::fmt::{self, Display, Formatter};

use dmg_core::{
    coverage::{CoverageGapKind, CoverageStatus},
    policy::{DecisionReasonKind, DecisionStatus},
    rehearsal::RehearsalOutcome,
    risk::{DatabaseObject, EvidenceKind, Finding, RelationLock, Severity},
    source::SourceSpan,
};

pub use diff::{write_diff_review, write_diff_text};
pub use evidence::EvidenceFormat;
pub use github::write_github_annotations;
pub use gitlab::write_gitlab_code_quality;
pub use html::write_html;
pub use markdown::write_markdown;
pub use rehearsal::write_rehearsal_review;
pub use sarif::write_sarif;

fn decision_name(status: Option<DecisionStatus>) -> &'static str {
    match status {
        Some(DecisionStatus::Pass) => "Pass",
        Some(DecisionStatus::Denied) => "Denied",
        Some(DecisionStatus::Incomplete) => "Incomplete",
        None => "Scan",
    }
}

fn coverage_name(status: CoverageStatus) -> &'static str {
    match status {
        CoverageStatus::Complete => "Complete",
        CoverageStatus::Incomplete => "Incomplete",
    }
}

fn reason_name(kind: DecisionReasonKind) -> &'static str {
    match kind {
        DecisionReasonKind::SeverityThreshold => "Severity threshold",
        DecisionReasonKind::IntegrityViolation => "Integrity violation",
        DecisionReasonKind::RequiredCoverage => "Required coverage",
        DecisionReasonKind::RequiredEvidence => "Required evidence",
        DecisionReasonKind::ApprovalRequired => "Approval required",
        DecisionReasonKind::RehearsalFailed => "Rehearsal failed",
    }
}

fn rehearsal_outcome_name(outcome: RehearsalOutcome) -> &'static str {
    match outcome {
        RehearsalOutcome::Succeeded => "Succeeded",
        RehearsalOutcome::Failed => "Failed",
        RehearsalOutcome::Incomplete => "Incomplete",
        RehearsalOutcome::TimedOut => "Timed out",
    }
}

fn lock_name(mode: RelationLock) -> &'static str {
    match mode {
        RelationLock::ShareUpdateExclusive => "SHARE UPDATE EXCLUSIVE",
        RelationLock::Share => "SHARE",
        RelationLock::ShareRowExclusive => "SHARE ROW EXCLUSIVE",
        RelationLock::AccessExclusive => "ACCESS EXCLUSIVE",
    }
}

fn evidence_name(kind: EvidenceKind) -> &'static str {
    match kind {
        EvidenceKind::InputFact => "Input fact",
        EvidenceKind::StaticPrediction => "Static prediction",
        EvidenceKind::CatalogEstimate => "Catalog estimate",
        EvidenceKind::SimulationObservation => "Simulation observation",
    }
}

fn source_label(span: &SourceSpan, max_path_bytes: usize) -> String {
    let path = bounded_line(span.path.as_str(), max_path_bytes);
    format!("{path}:{}:{}", span.start.line, span.start.column)
}

fn object_label(object: &DatabaseObject, max_component_bytes: usize) -> String {
    let mut name = String::new();
    for component in object
        .schema
        .iter()
        .chain([&object.name])
        .chain(&object.member)
    {
        if !name.is_empty() {
            name.push('.');
        }
        name.push_str(&bounded_line(component, max_component_bytes));
    }
    name
}

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
        CoverageGapKind::MissingCatalogEvidence => "Missing catalog evidence",
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

/// Escape control characters for a terminal or other line-oriented text review.
pub struct Terminal<'a>(pub &'a str);

impl Display for Terminal<'_> {
    fn fmt(&self, out: &mut Formatter<'_>) -> fmt::Result {
        for character in self.0.chars() {
            if character.is_control()
                || matches!(character, '\u{061c}' | '\u{200e}' | '\u{200f}' | '\u{2028}'..='\u{202e}' | '\u{2066}'..='\u{2069}')
            {
                write!(out, "{}", character.escape_unicode())?;
            } else {
                write!(out, "{character}")?;
            }
        }
        Ok(())
    }
}
