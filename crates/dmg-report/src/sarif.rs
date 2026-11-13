use std::{
    collections::{BTreeMap, BTreeSet},
    fmt::Write as _,
    io::{self, Write},
};

use dmg_core::{
    coverage::{CoverageStatus, DiagnosticScope},
    report::ReportV1,
    risk::{RuleId, Severity},
    source::{RepoPath, SourceSpan},
};
use serde::{Serialize, Serializer, ser::SerializeSeq};
use serde_json::{Value, json};

use super::{
    bounded_line, gap_name,
    machine::{message, valid_path, validate_locations},
};

const SCHEMA: &str =
    "https://docs.oasis-open.org/sarif/sarif/v2.1.0/errata01/os/schemas/sarif-schema-2.1.0.json";
const SOURCE_ROOT: &str = "%SRCROOT%";
const MAX_COVERAGE_NOTIFICATIONS: usize = 50;

/// Write SARIF 2.1.0 with repository-relative locations and Guard finding fingerprints.
pub fn write_sarif(report: &ReportV1, mut output: impl Write) -> io::Result<()> {
    validate_locations(report)?;
    let rule_ids: BTreeSet<_> = report
        .findings()
        .iter()
        .map(|finding| finding.rule_id)
        .collect();
    let rule_indexes = rule_ids
        .iter()
        .enumerate()
        .map(|(index, &id)| (id, index))
        .collect();
    let metadata = report.metadata();
    let rules = rule_ids
        .iter()
        .map(|id| json!({ "id": id }))
        .collect::<Vec<_>>();
    let tool = json!({
        "driver": {
            "name": "Database Migration Guard",
            "version": message(&metadata.tool_version, "unknown"),
            "rules": rules,
        }
    });
    // The consumer supplies the repository root. No local absolute path is recorded.
    let base_ids = json!({
        (SOURCE_ROOT): { "description": { "text": "Repository root" } }
    });
    let properties = json!({
        "dmgPostgresMajor": metadata.target,
        "dmgRulesetVersion": bounded_line(&metadata.ruleset_version, 64),
        "dmgCoverage": report.coverage().status(),
        "dmgCoverageGapCount": report.coverage().gaps().len(),
        "dmgDecision": report.decision().map(|decision| decision.status),
        "dmgPolicySha256": report.decision().map(|decision| decision.policy_digest),
        "dmgPolicyEvidenceSha256": report.policy_audit().map(|audit| audit.evidence_sha256),
        "dmgRehearsal": report.policy_audit().and_then(|audit| audit.rehearsal.as_ref()),
    });
    let log = Log {
        version: "2.1.0",
        schema: SCHEMA,
        runs: [Run {
            tool,
            column_kind: "unicodeCodePoints",
            newline_sequences: ["\r\n", "\n"],
            original_uri_base_ids: base_ids,
            invocations: [invocation(report)],
            properties,
            results: Results {
                report,
                rule_indexes: &rule_indexes,
            },
        }],
    };
    serde_json::to_writer(&mut output, &log)?;
    writeln!(output)
}

#[derive(Serialize)]
struct Log<'a> {
    version: &'static str,
    #[serde(rename = "$schema")]
    schema: &'static str,
    runs: [Run<'a>; 1],
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct Run<'a> {
    tool: Value,
    column_kind: &'static str,
    newline_sequences: [&'static str; 2],
    original_uri_base_ids: Value,
    invocations: [Value; 1],
    properties: Value,
    results: Results<'a>,
}

struct Results<'a> {
    report: &'a ReportV1,
    rule_indexes: &'a BTreeMap<RuleId, usize>,
}

impl Serialize for Results<'_> {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let mut results = serializer.serialize_seq(Some(self.report.findings().len()))?;
        for finding in self.report.findings() {
            let level = match finding.severity {
                Severity::Critical | Severity::High => "error",
                Severity::Medium => "warning",
                Severity::Low | Severity::Info => "note",
            };
            results.serialize_element(&json!({
                "ruleId": finding.rule_id,
                "ruleIndex": self.rule_indexes[&finding.rule_id],
                "level": level,
                "message": { "text": sarif_message(&finding.explanation, "PostgreSQL migration finding") },
                "locations": [location(&finding.span.path, Some(&finding.span))],
                "fingerprints": { "dmgFinding/v1": finding.fingerprint },
                "properties": {
                    "dmgSeverity": finding.severity,
                    "dmgConfidence": finding.confidence,
                    "dmgImpact": bounded_line(&finding.impact, 640),
                    "dmgMitigation": bounded_line(&finding.mitigation, 640),
                },
            }))?;
        }
        results.end()
    }
}

fn invocation(report: &ReportV1) -> Value {
    let gaps = report.coverage().gaps();
    let complete = report.coverage().status() == CoverageStatus::Complete;
    let mut notifications = Vec::new();
    if !complete {
        notifications.push(json!({
            "level": "warning",
            "message": { "text": format!("Database Migration Guard analysis is incomplete. Coverage gaps: {}.", gaps.len()) },
        }));
    }
    for gap in gaps.iter().take(MAX_COVERAGE_NOTIFICATIONS) {
        let scope = match &gap.scope {
            DiagnosticScope::Span { span } => Some((&span.path, Some(span))),
            DiagnosticScope::Path { path } => Some((path, None)),
            _ => None,
        };
        let mut notification = json!({
            "level": "warning",
            "message": {
                "text": format!("{} — {}", gap_name(gap.kind), sarif_message(&gap.message, "See the Guard JSON report.")),
            },
        });
        if let Some((path, span)) = scope.filter(|(path, _)| valid_path(path)) {
            notification["locations"] = json!([location(path, span)]);
        }
        notifications.push(notification);
    }
    let omitted = gaps.len().saturating_sub(MAX_COVERAGE_NOTIFICATIONS);
    if omitted > 0 {
        notifications.push(json!({
            "level": "warning",
            "message": { "text": format!("See the Guard JSON report for {omitted} more coverage gaps.") },
        }));
    }
    json!({
        "executionSuccessful": complete,
        "toolExecutionNotifications": notifications,
    })
}

fn sarif_message(value: &str, fallback: &str) -> String {
    let text = message(value, fallback);
    let mut escaped = String::with_capacity(text.len());
    for character in text.chars() {
        match character {
            '{' => escaped.push_str("{{"),
            '}' => escaped.push_str("}}"),
            '\\' | '[' | ']' => {
                escaped.push('\\');
                escaped.push(character);
            }
            _ => escaped.push(character),
        }
    }
    escaped
}

fn location(path: &RepoPath, span: Option<&SourceSpan>) -> Value {
    let mut physical = json!({
        "artifactLocation": {
            "uri": relative_uri(path),
            "uriBaseId": SOURCE_ROOT,
        }
    });
    if let Some(span) = span {
        let end = span.end.max(span.start);
        physical["region"] = json!({
            "startLine": span.start.line,
            "startColumn": span.start.column,
            "endLine": end.line,
            "endColumn": end.column,
        });
    }
    json!({ "physicalLocation": physical })
}

fn relative_uri(path: &RepoPath) -> String {
    let path = path.as_str();
    let mut uri = String::with_capacity(path.len());
    for byte in path.bytes() {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'.' | b'_' | b'~' | b'/') {
            uri.push(char::from(byte));
        } else {
            write!(uri, "%{byte:02X}").expect("writing to a String is infallible");
        }
    }
    uri
}
