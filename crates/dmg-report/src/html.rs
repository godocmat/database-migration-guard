use std::io::{self, Write};

use dmg_core::{
    coverage::{CoverageGap, DiagnosticScope},
    migration::{AppliedHistoryBasis, IntegrityBasis},
    policy::{PolicyAuditV1, PolicyDecision},
    report::ReportV1,
    risk::Finding,
};

use super::{
    bounded_line, coverage_name, decision_name, evidence_name, gap_name, lock_name, object_label,
    prioritized, reason_name, rehearsal_outcome_name, severity_name, source_label,
};

const MAX_FINDINGS: usize = 200;
const MAX_GAPS: usize = 50;
const MAX_REASONS: usize = 30;
const MAX_LOCKS: usize = 50;
const MAX_POLICY_MATCHES: usize = 30;
const MAX_EVIDENCE: usize = 3;
const MAX_RELATED_PATHS: usize = 3;
const MAX_TEXT_BYTES: usize = 640;
const MAX_PATH_BYTES: usize = 240;
const MAX_OBJECT_COMPONENT_BYTES: usize = 80;

const PAGE_START: &str = r#"<!doctype html>
<html lang="en">
<head>
<meta charset="utf-8">
<meta name="viewport" content="width=device-width, initial-scale=1">
<meta name="referrer" content="no-referrer">
<meta http-equiv="Content-Security-Policy" content="default-src 'none'; style-src 'unsafe-inline'; base-uri 'none'; form-action 'none'">
<title>Database Migration Guard</title>
<style>
:root { color-scheme: light dark; font-family: Inter, system-ui, sans-serif; line-height: 1.55; }
body { margin: 0; }
main { max-width: 60rem; margin: 0 auto; padding: 2rem 1rem; overflow-wrap: anywhere; }
h1 { margin: 0; font-size: clamp(1.65rem, 5vw, 2.4rem); line-height: 1.2; }
section { margin-block: 2rem; }
h2 { margin-block: 0 .65rem; font-size: 1.25rem; }
.status { margin-block: .65rem 1rem; font-size: 1.1rem; }
dl { display: grid; grid-template-columns: minmax(8rem, 1fr) minmax(0, 3fr); gap: .3rem 1rem; }
dt { font-weight: 600; }
dd { margin: 0; }
ul { padding-inline-start: 1.25rem; }
li + li { margin-top: .35rem; }
details { margin-top: .55rem; padding: .6rem .8rem; border: 1px solid; border-radius: .4rem; }
summary { cursor: pointer; }
summary code { font-size: .9em; }
details p, details ul { margin-block: .5rem; }
@media (max-width: 32rem) { dl { display: block; } dd { margin-block-end: .45rem; } }
</style>
</head>
<body>
<main>
<h1>Database Migration Guard</h1>"#;

/// Write a bounded, standalone HTML review of a saved Guard report.
pub fn write_html(report: &ReportV1, mut output: impl Write) -> io::Result<()> {
    writeln!(output, "{PAGE_START}")?;
    let metadata = report.metadata();
    let decision = report.decision().map(|decision| decision.status);
    writeln!(
        output,
        "<p class=\"status\"><strong>{}</strong> · {} coverage</p>",
        decision_name(decision),
        coverage_name(report.coverage().status())
    )?;
    writeln!(output, "<dl>")?;
    fact(&mut output, "PostgreSQL target", metadata.target)?;
    fact(&mut output, "Guard version", text(&metadata.tool_version))?;
    fact(
        &mut output,
        "Ruleset version",
        text(&metadata.ruleset_version),
    )?;
    fact(
        &mut output,
        "Migration files",
        report.migrations().inventory.len(),
    )?;
    let selected = report.migrations().selected_forwards.as_ref();
    fact(
        &mut output,
        "Forward migrations selected",
        selected.map_or_else(|| "Unresolved".into(), |paths| paths.len().to_string()),
    )?;
    if let Some(integrity) = &report.migrations().integrity {
        match &integrity.basis {
            IntegrityBasis::Git {
                base_commit,
                head_commit,
            } => {
                fact(
                    &mut output,
                    "Git base",
                    text(&bounded_line(base_commit, 64)),
                )?;
                fact(
                    &mut output,
                    "Git head",
                    text(&bounded_line(head_commit, 64)),
                )?;
            }
            IntegrityBasis::Manifest { digest } => {
                fact(&mut output, "Integrity manifest SHA-256", digest)?;
            }
        }
    }
    if let Some(history) = &report.migrations().applied_history {
        match &history.basis {
            AppliedHistoryBasis::Manifest { digest } => {
                fact(&mut output, "Applied manifest SHA-256", digest)?;
            }
            AppliedHistoryBasis::Native {
                digest,
                inventory_digest,
                ..
            } => {
                fact(&mut output, "Native history SHA-256", digest)?;
                fact(&mut output, "Forward inventory SHA-256", inventory_digest)?;
            }
        }
    }
    if let Some(decision) = report.decision() {
        fact(&mut output, "Policy SHA-256", decision.policy_digest)?;
    }
    writeln!(output, "</dl>")?;

    if let Some(decision) = report
        .decision()
        .filter(|decision| !decision.reasons.is_empty())
    {
        write_reasons(&mut output, decision)?;
    }
    if let Some(audit) = report.policy_audit() {
        write_policy_audit(&mut output, audit)?;
    }
    write_gaps(&mut output, report.coverage().gaps())?;
    write_findings(&mut output, report)?;
    write_locks(&mut output, report)?;
    writeln!(output, "</main>\n</body>\n</html>")
}

fn write_reasons(out: &mut impl Write, decision: &PolicyDecision) -> io::Result<()> {
    writeln!(out, "<section><h2>Gate reasons</h2><ul>")?;
    for reason in decision.reasons.iter().take(MAX_REASONS) {
        writeln!(
            out,
            "<li><strong>{}</strong> · {}</li>",
            reason_name(reason.kind),
            text(&reason.message)
        )?;
    }
    writeln!(out, "</ul>")?;
    omitted(out, decision.reasons.len(), MAX_REASONS, "gate reasons")?;
    writeln!(out, "</section>")
}

fn write_policy_audit(out: &mut impl Write, audit: &PolicyAuditV1) -> io::Result<()> {
    writeln!(out, "<section><h2>Policy evidence</h2><dl>")?;
    fact(out, "Evidence bundle SHA-256", audit.evidence_sha256)?;
    fact(out, "Repository", text(audit.repository_id.as_str()))?;
    fact(out, "Target", text(audit.target_id.as_str()))?;
    if let Some(commit) = &audit.baseline_reference_commit {
        fact(out, "Baseline commit", text(&bounded_line(commit, 64)))?;
    }
    fact(out, "Baseline matches", audit.baseline_matches.len())?;
    fact(out, "Approvals", audit.approvals.len())?;
    if let Some(rehearsal) = &audit.rehearsal {
        fact(
            out,
            "Rehearsal outcome",
            rehearsal_outcome_name(rehearsal.outcome),
        )?;
        fact(out, "Rehearsal report SHA-256", rehearsal.report_sha256)?;
        fact(out, "Rehearsal binding SHA-256", rehearsal.binding_sha256)?;
        fact(
            out,
            "Rehearsal completion (Unix time)",
            rehearsal.completed_at_unix_seconds,
        )?;
    }
    writeln!(out, "</dl>")?;
    if !audit.baseline_matches.is_empty() {
        writeln!(out, "<ul>")?;
        for matched in audit.baseline_matches.iter().take(MAX_POLICY_MATCHES) {
            writeln!(
                out,
                "<li>Baseline match <strong>{}</strong> · <code>{}</code></li>",
                matched.rule_id, matched.finding_fingerprint
            )?;
        }
        writeln!(out, "</ul>")?;
        omitted(
            out,
            audit.baseline_matches.len(),
            MAX_POLICY_MATCHES,
            "baseline matches",
        )?;
    }
    if !audit.approvals.is_empty() {
        writeln!(out, "<ul>")?;
        for approval in audit.approvals.iter().take(MAX_POLICY_MATCHES) {
            writeln!(
                out,
                "<li>Approved <strong>{}</strong> · <code>{}</code> · {} · expires at Unix time {}</li>",
                approval.rule_id,
                approval.finding_fingerprint,
                text(approval.approver.as_str()),
                approval.expires_at_unix_seconds
            )?;
        }
        writeln!(out, "</ul>")?;
        omitted(out, audit.approvals.len(), MAX_POLICY_MATCHES, "approvals")?;
    }
    writeln!(out, "</section>")
}

fn write_gaps(out: &mut impl Write, gaps: &[CoverageGap]) -> io::Result<()> {
    if gaps.is_empty() {
        return Ok(());
    }
    writeln!(out, "<section><h2>Coverage gaps</h2><ul>")?;
    for gap in gaps.iter().take(MAX_GAPS) {
        let scope = match &gap.scope {
            DiagnosticScope::Repository => "repository".to_owned(),
            DiagnosticScope::Migration { identity } => format!(
                "{} migration {}",
                identity.adapter.as_str(),
                bounded_line(&identity.version.to_string(), MAX_PATH_BYTES),
            ),
            DiagnosticScope::Path { path } => bounded_line(path.as_str(), MAX_PATH_BYTES),
            DiagnosticScope::Span { span } => source_label(span, MAX_PATH_BYTES),
        };
        write!(
            out,
            "<li><strong>{}</strong> · <code>{}</code> · {}",
            gap_name(gap.kind),
            text(&scope),
            text(&gap.message)
        )?;
        if !gap.related_paths.is_empty() {
            writeln!(out, "<ul>")?;
            for path in gap.related_paths.iter().take(MAX_RELATED_PATHS) {
                writeln!(
                    out,
                    "<li>Related path <code>{}</code></li>",
                    text(&bounded_line(path.as_str(), MAX_PATH_BYTES))
                )?;
            }
            let omitted = gap.related_paths.len().saturating_sub(MAX_RELATED_PATHS);
            if omitted > 0 {
                writeln!(
                    out,
                    "<li>{omitted} more related paths in the Guard JSON report.</li>"
                )?;
            }
            write!(out, "</ul>")?;
        }
        writeln!(out, "</li>")?;
    }
    writeln!(out, "</ul>")?;
    omitted(out, gaps.len(), MAX_GAPS, "coverage gaps")?;
    writeln!(out, "</section>")
}

fn write_findings(out: &mut impl Write, report: &ReportV1) -> io::Result<()> {
    let counts = report.severity_summary();
    writeln!(out, "<section><h2>Findings</h2>")?;
    writeln!(
        out,
        "<p>Critical {} · High {} · Medium {} · Low {} · Info {}</p>",
        counts.critical, counts.high, counts.medium, counts.low, counts.info
    )?;
    for finding in prioritized(report.findings()).take(MAX_FINDINGS) {
        write_finding(out, finding)?;
    }
    omitted(out, report.findings().len(), MAX_FINDINGS, "findings")?;
    writeln!(out, "</section>")
}

fn write_finding(out: &mut impl Write, finding: &Finding) -> io::Result<()> {
    write!(
        out,
        "<details><summary><strong>{} {}</strong> · <code>{}</code>",
        severity_name(finding.severity),
        finding.rule_id,
        text(&source_label(&finding.span, MAX_PATH_BYTES))
    )?;
    if let Some(object) = &finding.object {
        write!(
            out,
            " · <code>{}</code>",
            text(&object_label(object, MAX_OBJECT_COMPONENT_BYTES))
        )?;
    }
    writeln!(out, "</summary>")?;
    for (label, value) in [
        ("Finding", &finding.explanation),
        ("Impact", &finding.impact),
        ("Mitigation", &finding.mitigation),
    ] {
        if !value.is_empty() {
            writeln!(out, "<p><strong>{label}</strong> · {}</p>", text(value))?;
        }
    }
    if !finding.evidence.is_empty() {
        writeln!(out, "<ul>")?;
        for evidence in finding.evidence.iter().take(MAX_EVIDENCE) {
            writeln!(
                out,
                "<li><strong>{}</strong> · {}</li>",
                evidence_name(evidence.kind),
                text(&evidence.detail)
            )?;
        }
        writeln!(out, "</ul>")?;
        omitted(out, finding.evidence.len(), MAX_EVIDENCE, "evidence items")?;
    }
    writeln!(
        out,
        "<p>Fingerprint <code>{}</code></p></details>",
        finding.fingerprint
    )
}

fn write_locks(out: &mut impl Write, report: &ReportV1) -> io::Result<()> {
    let locks = report.relation_locks();
    if locks.is_empty() {
        return Ok(());
    }
    writeln!(out, "<section><h2>Predicted relation locks</h2><ul>")?;
    for lock in locks.iter().take(MAX_LOCKS) {
        writeln!(
            out,
            "<li><strong>{}</strong> on <code>{}</code> at <code>{}</code></li>",
            lock_name(lock.mode),
            text(&object_label(&lock.object, MAX_OBJECT_COMPONENT_BYTES)),
            text(&source_label(&lock.span, MAX_PATH_BYTES))
        )?;
    }
    writeln!(out, "</ul>")?;
    omitted(out, locks.len(), MAX_LOCKS, "lock predictions")?;
    writeln!(out, "</section>")
}

fn fact(out: &mut impl Write, label: &str, value: impl std::fmt::Display) -> io::Result<()> {
    writeln!(out, "<dt>{label}</dt><dd>{value}</dd>")
}

fn omitted(out: &mut impl Write, count: usize, limit: usize, label: &str) -> io::Result<()> {
    let more = count.saturating_sub(limit);
    if more > 0 {
        writeln!(out, "<p>{more} more {label} in the Guard JSON report.</p>")?;
    }
    Ok(())
}

// Only text nodes receive input strings. Tag, attribute, and style data are constant.
fn text(value: &str) -> String {
    let value = bounded_line(value, MAX_TEXT_BYTES);
    let mut escaped = String::with_capacity(value.len());
    for character in value.chars() {
        match character {
            '&' => escaped.push_str("&amp;"),
            '<' => escaped.push_str("&lt;"),
            '>' => escaped.push_str("&gt;"),
            '"' => escaped.push_str("&quot;"),
            '\'' => escaped.push_str("&#39;"),
            _ => escaped.push(character),
        }
    }
    escaped
}
