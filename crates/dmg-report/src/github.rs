use std::io::{self, Write};

use dmg_core::{
    coverage::DiagnosticScope,
    report::ReportV1,
    risk::Severity,
    source::{RepoPath, SourceSpan},
};

use crate::{bounded_line, gap_name, is_control, prioritized, severity_name};

const ANNOTATIONS_PER_LEVEL: usize = 10;
const MAX_MESSAGE_BYTES: usize = 640;
const MAX_PATH_BYTES: usize = 1024;

/// Emit GitHub Actions annotations for a report without evaluating the gate.
pub fn write_github_annotations(report: &ReportV1, mut output: impl Write) -> io::Result<()> {
    let mut counts = [0; 3];
    let mut omitted_findings = 0;
    let mut omitted_gaps = 0;

    for gap in report.coverage().gaps() {
        // Keep one warning slot for coverage and finding counts when truncated.
        if counts[Level::Warning as usize] == ANNOTATIONS_PER_LEVEL - 1 {
            omitted_gaps += 1;
            continue;
        }
        let location = match &gap.scope {
            DiagnosticScope::Span { span } => Some(Location::Span(span)),
            DiagnosticScope::Path { path } => Some(Location::Path(path)),
            _ => None,
        };
        write_annotation(
            &mut output,
            Level::Warning,
            gap_name(gap.kind),
            location,
            &gap.message,
        )?;
        counts[Level::Warning as usize] += 1;
    }
    for finding in prioritized(report.findings()) {
        let level = match finding.severity {
            Severity::Critical | Severity::High => Level::Error,
            Severity::Medium => Level::Warning,
            Severity::Low | Severity::Info => Level::Notice,
        };
        let limit = if level == Level::Warning {
            ANNOTATIONS_PER_LEVEL - 1
        } else {
            ANNOTATIONS_PER_LEVEL
        };
        let count = &mut counts[level as usize];
        if *count == limit {
            omitted_findings += 1;
            continue;
        }
        let title = format!("{} {}", severity_name(finding.severity), finding.rule_id);
        write_annotation(
            &mut output,
            level,
            &title,
            Some(Location::Span(&finding.span)),
            &finding.explanation,
        )?;
        *count += 1;
    }

    if omitted_findings > 0 || omitted_gaps > 0 {
        write_annotation(
            &mut output,
            Level::Warning,
            "Database Migration Guard",
            None,
            &format!(
                "Omitted {omitted_findings} findings and {omitted_gaps} coverage gaps. See the JSON report."
            ),
        )?;
    }
    Ok(())
}

#[derive(Clone, Copy, Eq, PartialEq)]
enum Level {
    Notice,
    Warning,
    Error,
}

impl Level {
    fn command(self) -> &'static str {
        match self {
            Self::Notice => "notice",
            Self::Warning => "warning",
            Self::Error => "error",
        }
    }
}

enum Location<'a> {
    Path(&'a RepoPath),
    Span(&'a SourceSpan),
}

fn write_annotation(
    output: &mut impl Write,
    level: Level,
    title: &str,
    location: Option<Location<'_>>,
    message: &str,
) -> io::Result<()> {
    write!(
        output,
        "::{} title={}",
        level.command(),
        escape_command(title, true)
    )?;
    if let Some(location) = location {
        let (path, span) = match location {
            Location::Path(path) => (path, None),
            Location::Span(span) => (&span.path, Some(span)),
        };
        let path = path.as_str();
        if path.len() <= MAX_PATH_BYTES && !path.chars().any(is_control) {
            write!(output, ",file={}", escape_command(path, true))?;
            if let Some(span) = span {
                write_position(output, span)?;
            }
        }
    }
    let message = bounded_line(message, MAX_MESSAGE_BYTES);
    writeln!(output, "::{}", escape_command(&message, false))
}

fn write_position(output: &mut impl Write, span: &SourceSpan) -> io::Result<()> {
    let Ok(line) = i32::try_from(span.start.line.get()) else {
        return Ok(());
    };
    write!(output, ",line={line}")?;
    // GitHub columns are supported for a single-line range. Report ends are exclusive.
    if span.start.line == span.end.line {
        if let Ok(column) = i32::try_from(span.start.column.get()) {
            let end = span.end.column.get().saturating_sub(1).max(column as usize);
            write!(output, ",col={column}")?;
            if let Ok(end) = i32::try_from(end) {
                write!(output, ",endColumn={end}")?;
            }
        }
    } else if span.start.line < span.end.line {
        let last = if span.end.column.get() == 1 {
            span.end.line.get() - 1
        } else {
            span.end.line.get()
        };
        if let Ok(last) = i32::try_from(last) {
            write!(output, ",endLine={last}")?;
        }
    }
    Ok(())
}

// Match the Actions Toolkit property and data escaping without re-escaping percent sequences.
fn escape_command(value: &str, property: bool) -> String {
    let mut escaped = String::with_capacity(value.len());
    for character in value.chars() {
        match character {
            '%' => escaped.push_str("%25"),
            '\r' => escaped.push_str("%0D"),
            '\n' => escaped.push_str("%0A"),
            ':' if property => escaped.push_str("%3A"),
            ',' if property => escaped.push_str("%2C"),
            _ => escaped.push(character),
        }
    }
    escaped
}
