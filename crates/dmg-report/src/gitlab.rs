use std::io::{self, Write};

use dmg_core::{report::ReportV1, risk::Severity};
use serde::{Serializer as _, ser::SerializeSeq as _};
use serde_json::json;

use super::machine::{message, validate_locations};

/// Write GitLab's Code Quality array from the report's findings.
pub fn write_gitlab_code_quality(report: &ReportV1, mut output: impl Write) -> io::Result<()> {
    validate_locations(report)?;
    let mut serializer = serde_json::Serializer::new(&mut output);
    let mut issues = serializer.serialize_seq(Some(report.findings().len()))?;
    for finding in report.findings() {
        let severity = match finding.severity {
            Severity::Info => "info",
            Severity::Low => "minor",
            Severity::Medium => "major",
            Severity::High => "critical",
            Severity::Critical => "blocker",
        };
        issues.serialize_element(&json!({
            "check_name": finding.rule_id,
            "description": message(&finding.explanation, "PostgreSQL migration finding"),
            "fingerprint": finding.fingerprint,
            "severity": severity,
            "location": {
                "path": finding.span.path,
                "lines": { "begin": finding.span.start.line },
            }
        }))?;
    }
    issues.end()?;
    writeln!(output)
}
