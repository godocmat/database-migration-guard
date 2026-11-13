use std::{
    fmt,
    io::{self, Write},
};

use dmg_core::{
    application::{
        ContractFact, ContractStatus, ContractTransition, ContractUnknown, ContractViolation,
        FactAssessment,
    },
    coverage::CoverageStatus,
    diff::{DiffGap, DiffSide, SchemaDiffV1, ScopeState, StructuralChange},
    inspection::InspectionGapScope,
    report::DiffReportV1,
    risk::{DatabaseObject, ObjectKind},
    snapshot::QualifiedName,
};

use super::{EvidenceFormat, Terminal, evidence::Review};

const MAX_CONTRACTS: usize = 16;

/// Render the native schema comparison text used by dmg diff.
pub fn write_diff_text(report: &DiffReportV1, output: impl Write) -> io::Result<()> {
    write_diff(report, Review::new(output, None))
}

/// Render a saved schema comparison and its application checks.
pub fn write_diff_review(
    report: &DiffReportV1,
    format: EvidenceFormat,
    output: impl Write,
) -> io::Result<()> {
    write_diff(report, Review::new(output, Some(format)))
}

fn write_diff(report: &DiffReportV1, mut out: Review<impl Write>) -> io::Result<()> {
    out.start("Schema comparison")?;
    out.line(format_args!(
        "Schema comparison is {}.",
        coverage_name(report.coverage())
    ))?;
    let comparison = report.comparison();
    write_comparison(
        "Drift (expected to current)",
        &comparison.expected_to_current,
        &mut out,
    )?;
    write_comparison(
        "Proposed changes (current to target)",
        &comparison.current_to_target,
        &mut out,
    )?;
    let assessments = report.application_contracts();
    if !assessments.is_empty() {
        out.section("Application contracts")?;
    }
    for assessment in assessments.iter().take(MAX_CONTRACTS) {
        out.line(format_args!(
            "Application {} {} — current {}, target {}.",
            Terminal(&assessment.application),
            Terminal(&assessment.version),
            status_name(assessment.current.status),
            status_name(assessment.target.status),
        ))?;
        if assessment.critical_target_violation {
            out.detail(format_args!("Critical target violation."))?;
        }
        let details = assessment.checks.iter().filter(|check| {
            check.current != FactAssessment::Satisfied || check.target != FactAssessment::Satisfied
        });
        out.details(details, |out, check| {
            let fact = match check.fact {
                ContractFact::Table { .. } => "table",
                ContractFact::ColumnType { .. } => "column type",
                ContractFact::ExplicitWrite => "explicit write",
                ContractFact::Nullability { .. } => "nullability",
            };
            let transition = match check.transition {
                ContractTransition::Preserved => "preserved",
                ContractTransition::Regression => "regression",
                ContractTransition::PreexistingViolation => "preexisting violation",
                ContractTransition::Resolved => "resolved",
                ContractTransition::Indeterminate => "indeterminate",
            };
            out.detail(format_args!(
                "{}{} {fact} — {transition}. Current {}, target {}.",
                Qualified(&check.table),
                Member(check.column.as_deref()),
                assessment_detail(&check.current),
                assessment_detail(&check.target),
            ))
        })?;
    }
    out.omitted(
        assessments.len().saturating_sub(MAX_CONTRACTS),
        "application contracts",
    )?;
    out.finish()
}

pub(super) fn write_comparison(
    label: &'static str,
    comparison: &SchemaDiffV1,
    out: &mut Review<impl Write>,
) -> io::Result<()> {
    out.summary(
        label,
        format_args!(
            "{}, {} changes, {} coverage gaps.",
            coverage_name(comparison.coverage),
            comparison.changes.len(),
            comparison.gaps.len()
        ),
    )?;
    let exceptional = comparison.schemas.iter().filter(|schema| {
        (schema.before, schema.after) != (ScopeState::Visible, ScopeState::Visible)
    });
    out.details(exceptional, |out, schema| {
        out.detail(format_args!(
            "Schema {} — {} to {}.",
            Identifier(&schema.schema),
            scope_name(schema.before),
            scope_name(schema.after),
        ))
    })?;
    out.details(comparison.changes.iter(), |out, change| match change {
        StructuralChange::Added { object } => {
            out.detail(format_args!("Added {}.", Object(&object.identity)))
        }
        StructuralChange::Removed { object } => {
            out.detail(format_args!("Removed {}.", Object(&object.identity)))
        }
        StructuralChange::Renamed { from, to } => out.detail(format_args!(
            "Renamed {} to {}.",
            Object(&from.identity),
            Object(&to.identity),
        )),
        StructuralChange::Modified { after, fields, .. } => out.detail(format_args!(
            "Modified {} ({:?}).",
            Object(&after.identity),
            &fields[..fields.len().min(32)],
        )),
        StructuralChange::PartitionAdded { edge } => out.detail(format_args!(
            "Added partition {} under {}.",
            Qualified(&edge.child),
            Qualified(&edge.parent),
        )),
        StructuralChange::PartitionRemoved { edge } => out.detail(format_args!(
            "Removed partition {} under {}.",
            Qualified(&edge.child),
            Qualified(&edge.parent),
        )),
        StructuralChange::PartitionModified { after, .. } => out.detail(format_args!(
            "Modified partition {} under {}.",
            Qualified(&after.child),
            Qualified(&after.parent),
        )),
        StructuralChange::DependencyAdded { edge } => out.detail(format_args!(
            "Added dependency from {} to {}.",
            Object(&edge.dependent),
            Object(&edge.referenced),
        )),
        StructuralChange::DependencyRemoved { edge } => out.detail(format_args!(
            "Removed dependency from {} to {}.",
            Object(&edge.dependent),
            Object(&edge.referenced),
        )),
    })?;
    out.details(comparison.gaps.iter(), |out, gap| match gap {
        DiffGap::EmptyScope => out.detail(format_args!("No schemas were supplied.")),
        DiffGap::SnapshotUnavailable { side } => {
            out.detail(format_args!("{} snapshot unavailable.", side_name(*side)))
        }
        DiffGap::Inspection { side, gap } => out.detail(format_args!(
            "{} {:?}{} — {:?}.",
            side_name(*side),
            gap.field,
            GapScope(&gap.scope),
            gap.reason,
        )),
        DiffGap::PresenceUnconfirmed {
            missing_from,
            object,
        } => out.detail(format_args!(
            "{} presence unconfirmed for {}.",
            side_name(*missing_from),
            Object(&object.identity),
        )),
        DiffGap::PartitionPresenceUnconfirmed { missing_from, edge } => out.detail(format_args!(
            "{} partition edge unconfirmed for {} under {}.",
            side_name(*missing_from),
            Qualified(&edge.child),
            Qualified(&edge.parent),
        )),
        DiffGap::DependencyPresenceUnconfirmed { missing_from, edge } => out.detail(format_args!(
            "{} dependency unconfirmed from {} to {}.",
            side_name(*missing_from),
            Object(&edge.dependent),
            Object(&edge.referenced),
        )),
        DiffGap::FieldUnavailable {
            side,
            object,
            field,
        } => out.detail(format_args!(
            "{} {field:?} unavailable for {}.",
            side_name(*side),
            Object(&object.identity),
        )),
    })
}

fn coverage_name(status: CoverageStatus) -> &'static str {
    match status {
        CoverageStatus::Complete => "complete",
        CoverageStatus::Incomplete => "incomplete",
    }
}

fn status_name(status: ContractStatus) -> &'static str {
    match status {
        ContractStatus::Satisfied => "satisfied",
        ContractStatus::Violated => "violated",
        ContractStatus::Unknown => "unknown",
    }
}

fn side_name(side: DiffSide) -> &'static str {
    match side {
        DiffSide::Before => "Before",
        DiffSide::After => "After",
    }
}

fn scope_name(state: ScopeState) -> &'static str {
    match state {
        ScopeState::Visible => "visible",
        ScopeState::Missing => "missing",
        ScopeState::Denied => "denied",
        ScopeState::Unavailable => "unavailable",
        ScopeState::OutsideScope => "outside scope",
    }
}

fn assessment_detail(assessment: &FactAssessment) -> &'static str {
    match assessment {
        FactAssessment::Satisfied => "satisfied",
        FactAssessment::Violated { reason } => match reason {
            ContractViolation::MissingTable => "table absent",
            ContractViolation::MissingColumn => "column absent",
            ContractViolation::TableKindMismatch { .. } => "table kind differs",
            ContractViolation::ColumnTypeMismatch { .. } => "column type differs",
            ContractViolation::MissingNotNull => "NOT NULL absent",
            ContractViolation::DeclaredNotNull => "NOT NULL declared",
            ContractViolation::GeneratedColumn => "generated column",
            ContractViolation::AlwaysIdentity => "GENERATED ALWAYS identity",
        },
        FactAssessment::Unknown { reason } => match reason {
            ContractUnknown::TableUnobserved { .. } => "table unobserved",
            ContractUnknown::ColumnUnobserved => "column unobserved",
            ContractUnknown::NotNullConstraintUnobserved => "NOT NULL constraint unobserved",
            ContractUnknown::NotNullUnvalidated => "NOT NULL unvalidated",
            ContractUnknown::NotNullUnenforced => "NOT NULL unenforced",
        },
    }
}

pub(super) struct Identifier<'a>(pub(super) &'a str);

impl fmt::Display for Identifier<'_> {
    fn fmt(&self, out: &mut fmt::Formatter<'_>) -> fmt::Result {
        out.write_str("\"")?;
        for part in self.0.split_inclusive('"') {
            write!(out, "{}", Terminal(part))?;
            if part.ends_with('"') {
                out.write_str("\"")?;
            }
        }
        out.write_str("\"")
    }
}

pub(super) struct Qualified<'a>(pub(super) &'a QualifiedName);

impl fmt::Display for Qualified<'_> {
    fn fmt(&self, out: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            out,
            "{}.{}",
            Identifier(&self.0.schema),
            Identifier(&self.0.name)
        )
    }
}

struct Object<'a>(&'a DatabaseObject);

impl fmt::Display for Object<'_> {
    fn fmt(&self, out: &mut fmt::Formatter<'_>) -> fmt::Result {
        let kind = match self.0.kind {
            ObjectKind::Schema => "schema",
            ObjectKind::Relation => "table",
            ObjectKind::Column => "column",
            ObjectKind::Index => "index",
            ObjectKind::Constraint => "constraint",
            ObjectKind::Type => "type",
        };
        write!(out, "{kind} ")?;
        if let Some(schema) = &self.0.schema {
            write!(out, "{}.", Identifier(schema))?;
        }
        write!(out, "{}", Identifier(&self.0.name))?;
        if let Some(member) = &self.0.member {
            write!(out, ".{}", Identifier(member))?;
        }
        Ok(())
    }
}

struct Member<'a>(Option<&'a str>);

impl fmt::Display for Member<'_> {
    fn fmt(&self, out: &mut fmt::Formatter<'_>) -> fmt::Result {
        if let Some(member) = self.0 {
            write!(out, ".{}", Identifier(member))?;
        }
        Ok(())
    }
}

struct GapScope<'a>(&'a InspectionGapScope);

impl fmt::Display for GapScope<'_> {
    fn fmt(&self, out: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.0 {
            InspectionGapScope::Target | InspectionGapScope::Migration { .. } => Ok(()),
            InspectionGapScope::Schema { schema } => {
                write!(out, " for schema {}", Identifier(schema))
            }
            InspectionGapScope::Object { object } => write!(out, " for {}", Object(object)),
        }
    }
}
