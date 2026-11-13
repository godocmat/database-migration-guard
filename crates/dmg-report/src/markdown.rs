use std::io::{self, Write};

use dmg_core::{
    coverage::DiagnosticScope,
    migration::{IntegrityBasis, MigrationAdapter},
    policy::PolicyAuditV1,
    report::ReportV1,
    risk::{DatabaseObject, Finding},
    source::SourceSpan,
};

use crate::{
    bounded_line, coverage_name, decision_name, evidence_name, gap_name, lock_name, object_label,
    prioritized, reason_name, rehearsal_outcome_name, severity_name, source_label,
};

const MAX_FINDINGS: usize = 30;
const MAX_GAPS: usize = 12;
const MAX_REASONS: usize = 12;
const MAX_LOCKS: usize = 12;
const MAX_POLICY_MATCHES: usize = 12;
const MAX_RELATED_PATHS: usize = 3;
const MAX_FINDING_EVIDENCE: usize = 3;
const MAX_TEXT_BYTES: usize = 360;
const MAX_IDENTIFIER_BYTES: usize = 240;

/// Render a bounded GitHub Flavored Markdown summary from a saved report.
pub fn write_markdown(report: &ReportV1, mut output: impl Write) -> io::Result<()> {
    let status = decision_name(report.decision().map(|decision| decision.status));
    let metadata = report.metadata();
    let coverage = coverage_name(report.coverage().status());
    writeln!(output, "## Database Migration Guard · {status}\n")?;
    writeln!(
        output,
        "- PostgreSQL {} · Guard {} · ruleset {}",
        metadata.target,
        literal(&metadata.tool_version, 64),
        literal(&metadata.ruleset_version, 64),
    )?;
    writeln!(output, "- Analysis coverage **{coverage}**")?;
    if let Some(catalog) = report.catalog_context() {
        writeln!(
            output,
            "- Catalog coverage **{}** · target {} · artifact SHA-256 `{}`",
            coverage_name(catalog.coverage),
            literal(catalog.observation.target_id.as_str(), 128),
            catalog.artifact_digest
        )?;
    }
    if let Some(paths) = &report.migrations().selected_forwards {
        writeln!(output, "- Forward migrations selected **{}**", paths.len())?;
    } else {
        writeln!(output, "- Forward migration selection is unresolved")?;
    }
    if let Some(integrity) = &report.migrations().integrity {
        match &integrity.basis {
            IntegrityBasis::Git {
                base_commit,
                head_commit,
            } => writeln!(
                output,
                "- Git base {} · head {}",
                literal(base_commit, 64),
                literal(head_commit, 64)
            )?,
            IntegrityBasis::Manifest { digest } => {
                writeln!(output, "- Integrity manifest SHA-256 `{digest}`")?;
            }
        }
    }

    if let Some(decision) = report.decision() {
        writeln!(output, "- Policy SHA-256 `{}`", decision.policy_digest)?;
        if !decision.reasons.is_empty() {
            writeln!(output, "\n### Gate reasons\n")?;
            for reason in decision.reasons.iter().take(MAX_REASONS) {
                let kind = reason_name(reason.kind);
                writeln!(output, "- **{kind}** · {}", prose(&reason.message))?;
            }
            write_omitted(
                &mut output,
                decision.reasons.len(),
                MAX_REASONS,
                "gate reasons",
            )?;
        }
    }

    if let Some(audit) = report.policy_audit() {
        write_policy_audit(&mut output, audit)?;
    }

    let gaps = report.coverage().gaps();
    if !gaps.is_empty() {
        writeln!(output, "\n### Coverage gaps\n")?;
        for gap in gaps.iter().take(MAX_GAPS) {
            let scope = match &gap.scope {
                DiagnosticScope::Repository => "repository".to_owned(),
                DiagnosticScope::Migration { identity } => {
                    let adapter = match identity.adapter {
                        MigrationAdapter::Plain => "plain SQL",
                        MigrationAdapter::Sqlx => "SQLx",
                        MigrationAdapter::Flyway => "Flyway",
                        MigrationAdapter::Goose => "Goose",
                        MigrationAdapter::Supabase => "Supabase",
                    };
                    let version = match identity.version.as_numeric() {
                        Some(version) => version.to_string(),
                        None => literal(&identity.version.to_string(), MAX_IDENTIFIER_BYTES),
                    };
                    format!("{adapter} migration {version}")
                }
                DiagnosticScope::Path { path } => literal(path.as_str(), MAX_IDENTIFIER_BYTES),
                DiagnosticScope::Span { span } => location(span),
            };
            writeln!(
                output,
                "- **{}** · {scope} · {}",
                gap_name(gap.kind),
                prose(&gap.message)
            )?;
            if !gap.related_paths.is_empty() {
                write!(output, "  - Related")?;
                for path in gap.related_paths.iter().take(MAX_RELATED_PATHS) {
                    write!(
                        output,
                        " · {}",
                        literal(path.as_str(), MAX_IDENTIFIER_BYTES)
                    )?;
                }
                let omitted = gap.related_paths.len().saturating_sub(MAX_RELATED_PATHS);
                if omitted > 0 {
                    write!(output, " · {omitted} more in JSON")?;
                }
                writeln!(output)?;
            }
        }
        write_omitted(&mut output, gaps.len(), MAX_GAPS, "coverage gaps")?;
    }

    writeln!(output, "\n### Findings\n")?;
    let counts = report.severity_summary();
    writeln!(
        output,
        "Critical {} · High {} · Medium {} · Low {} · Info {}",
        counts.critical, counts.high, counts.medium, counts.low, counts.info
    )?;
    if !report.findings().is_empty() {
        writeln!(output)?;
        for finding in prioritized(report.findings()).take(MAX_FINDINGS) {
            write_finding(&mut output, finding)?;
        }
        write_omitted(
            &mut output,
            report.findings().len(),
            MAX_FINDINGS,
            "findings",
        )?;
    }

    let locks = report.relation_locks();
    if !locks.is_empty() {
        writeln!(output, "\n### Predicted relation locks\n")?;
        for lock in locks.iter().take(MAX_LOCKS) {
            let mode = lock_name(lock.mode);
            writeln!(
                output,
                "- **{mode}** on {} at {}",
                object_name(&lock.object),
                location(&lock.span)
            )?;
        }
        write_omitted(&mut output, locks.len(), MAX_LOCKS, "lock predictions")?;
    }
    Ok(())
}

fn write_policy_audit(output: &mut impl Write, audit: &PolicyAuditV1) -> io::Result<()> {
    writeln!(output, "\n### Policy evidence\n")?;
    writeln!(
        output,
        "- Evidence bundle SHA-256 `{}`",
        audit.evidence_sha256
    )?;
    writeln!(
        output,
        "- Repository {} · target {}",
        literal(audit.repository_id.as_str(), 128),
        literal(audit.target_id.as_str(), 128)
    )?;
    if let Some(commit) = &audit.baseline_reference_commit {
        writeln!(output, "- Baseline commit {}", literal(commit, 64))?;
    }
    for matched in audit.baseline_matches.iter().take(MAX_POLICY_MATCHES) {
        writeln!(
            output,
            "- Baseline match **{}** · `{}`",
            matched.rule_id, matched.finding_fingerprint
        )?;
    }
    write_omitted(
        output,
        audit.baseline_matches.len(),
        MAX_POLICY_MATCHES,
        "baseline matches",
    )?;
    for approval in audit.approvals.iter().take(MAX_POLICY_MATCHES) {
        writeln!(
            output,
            "- Approved **{}** · `{}` · {} · expires at Unix time {}",
            approval.rule_id,
            approval.finding_fingerprint,
            literal(approval.approver.as_str(), 128),
            approval.expires_at_unix_seconds
        )?;
    }
    write_omitted(
        output,
        audit.approvals.len(),
        MAX_POLICY_MATCHES,
        "approvals",
    )?;
    if let Some(rehearsal) = &audit.rehearsal {
        writeln!(
            output,
            "- Rehearsal **{}** · report SHA-256 `{}` · binding SHA-256 `{}` · completed at Unix time {}",
            rehearsal_outcome_name(rehearsal.outcome),
            rehearsal.report_sha256,
            rehearsal.binding_sha256,
            rehearsal.completed_at_unix_seconds
        )?;
    }
    Ok(())
}

fn write_finding(output: &mut impl Write, finding: &Finding) -> io::Result<()> {
    write!(
        output,
        "- **{} {}** at {}",
        severity_name(finding.severity),
        finding.rule_id,
        location(&finding.span)
    )?;
    if let Some(object) = &finding.object {
        write!(output, " on {}", object_name(object))?;
    }
    writeln!(output)?;
    if !finding.explanation.is_empty() {
        writeln!(output, "  - **Finding** · {}", prose(&finding.explanation))?;
    }
    if !finding.impact.is_empty() {
        writeln!(output, "  - **Impact** · {}", prose(&finding.impact))?;
    }
    for evidence in finding.evidence.iter().take(MAX_FINDING_EVIDENCE) {
        let label = evidence_name(evidence.kind);
        writeln!(output, "  - **{label}** · {}", prose(&evidence.detail))?;
    }
    let omitted = finding.evidence.len().saturating_sub(MAX_FINDING_EVIDENCE);
    if omitted > 0 {
        writeln!(output, "  - {omitted} more evidence items in JSON.")?;
    }
    if !finding.mitigation.is_empty() {
        writeln!(
            output,
            "  - **Mitigation** · {}",
            prose(&finding.mitigation)
        )?;
    }
    Ok(())
}

fn write_omitted(
    output: &mut impl Write,
    count: usize,
    limit: usize,
    label: &str,
) -> io::Result<()> {
    let omitted = count.saturating_sub(limit);
    if omitted > 0 {
        writeln!(output, "\n{omitted} more {label} in the JSON report.")?;
    }
    Ok(())
}

fn location(span: &SourceSpan) -> String {
    code_span(&source_label(span, MAX_IDENTIFIER_BYTES))
}

fn object_name(object: &DatabaseObject) -> String {
    code_span(&object_label(object, MAX_IDENTIFIER_BYTES / 3))
}

fn literal(value: &str, max_bytes: usize) -> String {
    code_span(&bounded_line(value, max_bytes))
}

fn code_span(line: &str) -> String {
    if line.trim().is_empty() {
        return "`—`".to_owned();
    }
    let mut longest = 0;
    let mut run = 0;
    for byte in line.bytes() {
        if byte == b'`' {
            run += 1;
            longest = longest.max(run);
        } else {
            run = 0;
        }
    }
    let fence = "`".repeat(longest + 1);
    if line.starts_with(['`', ' ']) || line.ends_with(['`', ' ']) {
        format!("{fence} {line} {fence}")
    } else {
        format!("{fence}{line}{fence}")
    }
}

fn prose(value: &str) -> String {
    escape_prose(&bounded_line(value, MAX_TEXT_BYTES))
}

pub(super) fn escape_prose(value: &str) -> String {
    let line = value.trim();
    // GFM autolinks and GitHub references can also arise in plain text.
    if line.contains(['@', '#'])
        || line.contains("://")
        || line
            .as_bytes()
            .windows(4)
            .any(|word| word.eq_ignore_ascii_case(b"www."))
    {
        return code_span(line);
    }
    let mut escaped = String::with_capacity(line.len());
    for character in line.chars() {
        if matches!(
            character,
            '\\' | '`' | '*' | '_' | '{' | '}' | '[' | ']' | '<' | '>' | '|' | '&' | '~' | '$'
        ) {
            escaped.push('\\');
        }
        escaped.push(character);
    }
    escaped
}
