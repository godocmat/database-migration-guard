use std::{
    fmt::{self, Display, Formatter},
    io::{self, BufWriter, Write},
    path::Path,
};

use dmg_core::{
    coverage::{CoverageStatus, DiagnosticScope},
    policy::DecisionStatus,
    report::ReportV1,
    risk::{Confidence, DatabaseObject, EvidenceKind, RelationLock, Severity},
    source::SourceSpan,
};
use tempfile::NamedTempFile;

pub(crate) fn write_report(
    path: Option<&Path>,
    render: impl FnOnce(&mut dyn Write) -> io::Result<()>,
) -> io::Result<()> {
    match path {
        Some(path) => write_file(path, true, render),
        None => {
            let mut output = BufWriter::new(io::stdout().lock());
            render(&mut output)?;
            output.flush()
        }
    }
}

pub(crate) fn write_file(
    path: &Path,
    replace: bool,
    render: impl FnOnce(&mut dyn Write) -> io::Result<()>,
) -> io::Result<()> {
    let directory = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    let mut temporary = NamedTempFile::new_in(directory)?;
    {
        let mut output = BufWriter::new(temporary.as_file_mut());
        render(&mut output)?;
        output.flush()?;
    }
    temporary.as_file().sync_all()?;
    let persisted = if replace {
        temporary.persist(path)
    } else {
        temporary.persist_noclobber(path)
    };
    persisted.map(|_| ()).map_err(|error| error.error)
}

pub(crate) fn write_json(report: &ReportV1, mut out: impl Write) -> io::Result<()> {
    serde_json::to_writer_pretty(&mut out, report)?;
    writeln!(out)
}

pub(crate) fn write_text(report: &ReportV1, mut out: impl Write) -> io::Result<()> {
    let coverage = match report.coverage().status() {
        CoverageStatus::Complete => "complete",
        CoverageStatus::Incomplete => "incomplete",
    };
    let selected = report.migrations().selected_forwards.as_ref();
    write!(
        out,
        "PostgreSQL {} analysis is {coverage}",
        report.metadata().target
    )?;
    match selected {
        Some(paths) => writeln!(out, " ({} forward migrations selected).", paths.len())?,
        None => writeln!(out, " (forward selection unresolved).")?,
    }
    if let Some(decision) = report.decision() {
        let status = match decision.status {
            DecisionStatus::Pass => "passed",
            DecisionStatus::Denied => "denied",
            DecisionStatus::Incomplete => "incomplete",
        };
        writeln!(
            out,
            "Policy check {status}. Policy SHA-256 {}",
            decision.policy_digest
        )?;
        for reason in &decision.reasons {
            writeln!(out, "  {}", Terminal(&reason.message))?;
        }
    }
    for gap in report.coverage().gaps() {
        write!(out, "Coverage gap at ")?;
        match &gap.scope {
            DiagnosticScope::Repository => write!(out, "repository")?,
            DiagnosticScope::Migration { identity } => {
                write!(out, "migration {}", Terminal(&identity.version.to_string()))?
            }
            DiagnosticScope::Path { path } => write!(out, "{}", Terminal(path.as_str()))?,
            DiagnosticScope::Span { span } => write!(out, "{}", Location(span))?,
        }
        writeln!(out, " — {}", Terminal(&gap.message))?;
        for path in &gap.related_paths {
            writeln!(out, "  Related path {}", Terminal(path.as_str()))?;
        }
    }
    for finding in report.findings() {
        writeln!(
            out,
            "{} {} {}",
            Location(&finding.span),
            severity_name(finding.severity),
            finding.rule_id
        )?;
        if let Some(object) = &finding.object {
            writeln!(out, "  Object {}", Object(object))?;
        }
        let confidence = match finding.confidence {
            Confidence::Low => "low",
            Confidence::Medium => "medium",
            Confidence::High => "high",
        };
        writeln!(
            out,
            "  {} (confidence {confidence})",
            Terminal(&finding.explanation)
        )?;
        writeln!(out, "  Impact {}", Terminal(&finding.impact))?;
        writeln!(out, "  Mitigation {}", Terminal(&finding.mitigation))?;
        for evidence in &finding.evidence {
            let provenance = match evidence.kind {
                EvidenceKind::InputFact => "Input fact",
                EvidenceKind::StaticPrediction => "Static prediction",
                EvidenceKind::CatalogEstimate => "Catalog estimate",
                EvidenceKind::SimulationObservation => "Simulation observation",
            };
            writeln!(out, "  {provenance} — {}", Terminal(&evidence.detail))?;
        }
        writeln!(out, "  {}", Terminal(&finding.documentation))?;
    }
    for lock in report.relation_locks() {
        let mode = match lock.mode {
            RelationLock::ShareUpdateExclusive => "SHARE UPDATE EXCLUSIVE",
            RelationLock::Share => "SHARE",
            RelationLock::ShareRowExclusive => "SHARE ROW EXCLUSIVE",
            RelationLock::AccessExclusive => "ACCESS EXCLUSIVE",
        };
        writeln!(
            out,
            "{} predicted {mode} lock on {}",
            Location(&lock.span),
            Object(&lock.object)
        )?;
    }
    let count = report.severity_summary();
    writeln!(
        out,
        "Findings: {} critical, {} high, {} medium, {} low, {} info.",
        count.critical, count.high, count.medium, count.low, count.info
    )
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

struct Location<'a>(&'a SourceSpan);

impl Display for Location<'_> {
    fn fmt(&self, out: &mut Formatter<'_>) -> fmt::Result {
        write!(
            out,
            "{}:{}:{}",
            Terminal(self.0.path.as_str()),
            self.0.start.line,
            self.0.start.column
        )
    }
}

struct Object<'a>(&'a DatabaseObject);

impl Display for Object<'_> {
    fn fmt(&self, out: &mut Formatter<'_>) -> fmt::Result {
        if let Some(schema) = &self.0.schema {
            write!(out, "{}.", Terminal(schema))?;
        }
        write!(out, "{}", Terminal(&self.0.name))?;
        if let Some(member) = &self.0.member {
            write!(out, ".{}", Terminal(member))?;
        }
        Ok(())
    }
}

pub(crate) struct Terminal<'a>(pub(crate) &'a str);

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
