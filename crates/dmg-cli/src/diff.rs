use std::{
    fmt,
    io::{self, Write},
    path::{Path, PathBuf},
};

use clap::Args;
use dmg_core::{
    application::{
        ApplicationAssessmentV1, ApplicationContractV1, ContractFact, ContractStatus,
        ContractTransition, ContractUnknown, ContractViolation, FactAssessment,
        MAX_APPLICATION_CONTRACT_BYTES, evaluate_contract,
    },
    coverage::CoverageStatus,
    diff::{
        DiffError, DiffGap, DiffSide, RenameMapping, SchemaComparisonV1, SchemaDiffV1, SchemaInput,
        ScopeState, StructuralChange, compare_schemas,
    },
    inspection::{InspectionEvidenceV1, InspectionGapScope},
    risk::{DatabaseObject, ObjectKind},
    snapshot::{QualifiedName, SchemaSnapshotV1},
};
use serde::{
    Deserialize, Deserializer, Serialize,
    de::{self, IgnoredAny, MapAccess, Visitor},
};

use crate::{
    Failure, OPERATIONAL_FAILURE, ReportFormat,
    config::{InputError, read_input},
    render::{Terminal, write_report},
};

const MAX_SCHEMA_BYTES: u64 = 32 * 1024 * 1024;
const MAX_RENAME_BYTES: u64 = 1024 * 1024;
const MAX_RENAMES: usize = 4096;
const MAX_CONTRACTS: usize = 16;
const TEXT_DETAILS: usize = 20;

#[derive(Args)]
pub(crate) struct DiffArgs {
    /// Expected schema snapshot or inspection JSON.
    #[arg(long, value_name = "FILE")]
    expected: PathBuf,

    /// Current schema snapshot or inspection JSON.
    #[arg(long, value_name = "FILE")]
    current: PathBuf,

    /// Proposed target schema snapshot or inspection JSON.
    #[arg(long, value_name = "FILE")]
    target: PathBuf,

    /// Reviewed rename mappings from expected to current.
    #[arg(long, value_name = "FILE")]
    drift_renames: Option<PathBuf>,

    /// Reviewed rename mappings from current to target.
    #[arg(long, value_name = "FILE")]
    proposed_renames: Option<PathBuf>,

    /// Trusted application schema contract. Repeat for each deployed version.
    #[arg(long = "application-contract", value_name = "FILE")]
    application_contracts: Vec<PathBuf>,

    /// Report output format.
    #[arg(long, value_enum, default_value = "text")]
    format: ReportFormat,

    /// Write the report atomically to a file instead of stdout.
    #[arg(long, value_name = "FILE")]
    output: Option<PathBuf>,
}

#[derive(Serialize)]
struct DiffReport {
    #[serde(flatten)]
    comparison: SchemaComparisonV1,
    coverage: CoverageStatus,
    application_contracts: Vec<ApplicationAssessmentV1>,
}

pub(crate) fn run(args: DiffArgs) -> Result<u8, Failure> {
    let contracts = load_contracts(&args.application_contracts)?;
    let expected = load_schema(&args.expected, "expected schema")?;
    let current = load_schema(&args.current, "current schema")?;
    let target = load_schema(&args.target, "target schema")?;
    let drift_renames = load_renames(args.drift_renames.as_deref(), "drift rename mappings")?;
    let proposed_renames =
        load_renames(args.proposed_renames.as_deref(), "proposed rename mappings")?;

    let current = current.input();
    let target = target.input();
    let comparison = compare_schemas(
        expected.input(),
        current,
        target,
        &drift_renames,
        &proposed_renames,
    )
    .map_err(|error| match error {
        DiffError::Digest(_) => Failure::operational(error.to_string(), OPERATIONAL_FAILURE),
        _ => Failure::invalid(error.to_string()),
    })?;

    let application_contracts = contracts
        .iter()
        .map(|contract| evaluate_contract(contract, current, target))
        .collect::<Result<Vec<_>, _>>()
        .map_err(|_| {
            Failure::operational(
                "could not evaluate application contracts",
                OPERATIONAL_FAILURE,
            )
        })?;

    let complete = comparison.expected_to_current.coverage == CoverageStatus::Complete
        && comparison.current_to_target.coverage == CoverageStatus::Complete
        && application_contracts.iter().all(|assessment| {
            assessment.current.coverage == CoverageStatus::Complete
                && assessment.target.coverage == CoverageStatus::Complete
        });
    let report = DiffReport {
        comparison,
        coverage: if complete {
            CoverageStatus::Complete
        } else {
            CoverageStatus::Incomplete
        },
        application_contracts,
    };

    write_report(args.output.as_deref(), |mut out| match args.format {
        ReportFormat::Text => write_text(&report, out),
        ReportFormat::Json => {
            serde_json::to_writer_pretty(&mut out, &report)?;
            writeln!(out)
        }
    })
    .map_err(|_| Failure::operational("could not write the schema diff", OPERATIONAL_FAILURE))?;

    Ok(if complete { 0 } else { OPERATIONAL_FAILURE })
}

enum SavedSchema {
    Snapshot(SchemaSnapshotV1),
    Inspection(Box<InspectionEvidenceV1>),
}

impl SavedSchema {
    fn input(&self) -> SchemaInput<'_> {
        match self {
            Self::Snapshot(snapshot) => SchemaInput::Snapshot(snapshot),
            Self::Inspection(inspection) => SchemaInput::Inspection(inspection),
        }
    }
}

fn load_schema(path: &Path, label: &str) -> Result<SavedSchema, Failure> {
    let bytes = load_file(path, label, MAX_SCHEMA_BYTES)?;
    match parse_json::<SchemaKind>(&bytes, label)? {
        SchemaKind::Snapshot => parse_json(&bytes, label).map(SavedSchema::Snapshot),
        SchemaKind::Inspection => parse_json(&bytes, label).map(SavedSchema::Inspection),
    }
}

enum SchemaKind {
    Snapshot,
    Inspection,
}

impl<'de> Deserialize<'de> for SchemaKind {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct Shape;

        impl<'de> Visitor<'de> for Shape {
            type Value = SchemaKind;

            fn expecting(&self, out: &mut fmt::Formatter<'_>) -> fmt::Result {
                out.write_str("a schema snapshot or inspection evidence object")
            }

            fn visit_map<A: MapAccess<'de>>(self, mut fields: A) -> Result<Self::Value, A::Error> {
                let mut snapshot = false;
                let mut inspection = false;
                while let Some(field) = fields.next_key::<String>()? {
                    match field.as_str() {
                        "schemas" | "tables" | "partition_edges" | "dependencies" => {
                            snapshot = true
                        }
                        "observation" | "scope" | "snapshot" | "relation_estimates" | "history" => {
                            inspection = true
                        }
                        _ => (),
                    }
                    fields.next_value::<IgnoredAny>()?;
                }
                match (snapshot, inspection) {
                    (true, false) => Ok(SchemaKind::Snapshot),
                    (false, true) => Ok(SchemaKind::Inspection),
                    _ => Err(de::Error::custom(
                        "ambiguous or unrecognized schema artifact",
                    )),
                }
            }
        }

        // Inspect the top-level keys without building a second catalog object in memory.
        // Decode the chosen type from the original bytes so duplicate fields stay invalid.
        deserializer.deserialize_map(Shape)
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RenameFile {
    schema_version: u32,
    renames: Vec<RenameMapping>,
}

fn load_renames(path: Option<&Path>, label: &str) -> Result<Vec<RenameMapping>, Failure> {
    let Some(path) = path else {
        return Ok(Vec::new());
    };
    let bytes = load_file(path, label, MAX_RENAME_BYTES)?;
    let input: RenameFile = parse_json(&bytes, label)?;
    if input.schema_version != 1 {
        return Err(Failure::invalid(format!(
            "{label} require schema_version = 1"
        )));
    }
    if input.renames.len() > MAX_RENAMES {
        return Err(Failure::invalid(format!(
            "{label} exceed the {MAX_RENAMES} mapping limit"
        )));
    }
    Ok(input.renames)
}

fn load_contracts(paths: &[PathBuf]) -> Result<Vec<ApplicationContractV1>, Failure> {
    if paths.len() > MAX_CONTRACTS {
        return Err(Failure::invalid(format!(
            "schema diff accepts up to {MAX_CONTRACTS} application contracts"
        )));
    }
    let mut contracts = Vec::with_capacity(paths.len());
    for (index, path) in paths.iter().enumerate() {
        let label = format!("application contract {}", index + 1);
        let bytes = load_file(path, &label, MAX_APPLICATION_CONTRACT_BYTES as u64)?;
        let contract = ApplicationContractV1::from_json(bytes)
            .map_err(|_| Failure::invalid(format!("invalid versioned {label} JSON")))?;
        contracts.push(contract);
    }
    contracts.sort_unstable_by(|a, b| {
        (a.application(), a.version()).cmp(&(b.application(), b.version()))
    });
    if contracts.windows(2).any(|pair| {
        pair[0].application() == pair[1].application() && pair[0].version() == pair[1].version()
    }) {
        return Err(Failure::invalid(
            "application contracts repeat an application and version",
        ));
    }
    Ok(contracts)
}

fn load_file(path: &Path, label: &str, limit: u64) -> Result<Vec<u8>, Failure> {
    read_input(path, limit).map_err(|error| match error {
        InputError::Read(kind) => Failure::operational(
            format!("could not read {label} ({kind})"),
            OPERATIONAL_FAILURE,
        ),
        InputError::NotFile => Failure::invalid(format!("{label} must be a regular file")),
        InputError::Limit => Failure::invalid(format!("{label} exceeds the {limit}-byte limit")),
    })
}

fn parse_json<T: serde::de::DeserializeOwned>(bytes: &[u8], label: &str) -> Result<T, Failure> {
    serde_json::from_slice(bytes).map_err(|error| {
        Failure::invalid(format!(
            "invalid versioned {label} JSON at line {}, column {}",
            error.line(),
            error.column()
        ))
    })
}

fn write_text(report: &DiffReport, mut out: impl Write) -> io::Result<()> {
    writeln!(
        out,
        "Schema comparison is {}.",
        coverage_name(report.coverage)
    )?;
    write_comparison(
        "Drift (expected to current)",
        &report.comparison.expected_to_current,
        &mut out,
    )?;
    write_comparison(
        "Proposed changes (current to target)",
        &report.comparison.current_to_target,
        &mut out,
    )?;
    for assessment in &report.application_contracts {
        writeln!(
            out,
            "Application {} {} — current {}, target {}.",
            Terminal(&assessment.application),
            Terminal(&assessment.version),
            status_name(assessment.current.status),
            status_name(assessment.target.status),
        )?;
        if assessment.critical_target_violation {
            writeln!(out, "  Critical target violation.")?;
        }
        let details = assessment.checks.iter().filter(|check| {
            check.current != FactAssessment::Satisfied || check.target != FactAssessment::Satisfied
        });
        write_details(details, &mut out, |out, check| {
            write!(out, "  {}", Qualified(&check.table))?;
            if let Some(column) = &check.column {
                write!(out, ".{}", Identifier(column))?;
            }
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
            writeln!(
                out,
                " {fact} — {transition}. Current {}, target {}.",
                assessment_detail(&check.current),
                assessment_detail(&check.target),
            )
        })?;
    }
    Ok(())
}

fn write_comparison(label: &str, comparison: &SchemaDiffV1, mut out: impl Write) -> io::Result<()> {
    writeln!(
        out,
        "{label} — {}, {} changes, {} coverage gaps.",
        coverage_name(comparison.coverage),
        comparison.changes.len(),
        comparison.gaps.len()
    )?;
    for schema in &comparison.schemas {
        if (schema.before, schema.after) != (ScopeState::Visible, ScopeState::Visible) {
            writeln!(
                out,
                "  Schema {} — {} to {}.",
                Identifier(&schema.schema),
                scope_name(schema.before),
                scope_name(schema.after),
            )?;
        }
    }
    write_details(
        comparison.changes.iter(),
        &mut out,
        |out, change| match change {
            StructuralChange::Added { object } => {
                writeln!(out, "  Added {}.", Object(&object.identity))
            }
            StructuralChange::Removed { object } => {
                writeln!(out, "  Removed {}.", Object(&object.identity))
            }
            StructuralChange::Renamed { from, to } => writeln!(
                out,
                "  Renamed {} to {}.",
                Object(&from.identity),
                Object(&to.identity),
            ),
            StructuralChange::Modified { after, fields, .. } => {
                writeln!(out, "  Modified {} ({fields:?}).", Object(&after.identity))
            }
            StructuralChange::PartitionAdded { edge } => writeln!(
                out,
                "  Added partition {} under {}.",
                Qualified(&edge.child),
                Qualified(&edge.parent)
            ),
            StructuralChange::PartitionRemoved { edge } => writeln!(
                out,
                "  Removed partition {} under {}.",
                Qualified(&edge.child),
                Qualified(&edge.parent)
            ),
            StructuralChange::PartitionModified { after, .. } => writeln!(
                out,
                "  Modified partition {} under {}.",
                Qualified(&after.child),
                Qualified(&after.parent)
            ),
            StructuralChange::DependencyAdded { edge } => writeln!(
                out,
                "  Added dependency from {} to {}.",
                Object(&edge.dependent),
                Object(&edge.referenced)
            ),
            StructuralChange::DependencyRemoved { edge } => writeln!(
                out,
                "  Removed dependency from {} to {}.",
                Object(&edge.dependent),
                Object(&edge.referenced)
            ),
        },
    )?;
    write_details(comparison.gaps.iter(), &mut out, |out, gap| match gap {
        DiffGap::EmptyScope => writeln!(out, "  No schemas were supplied."),
        DiffGap::SnapshotUnavailable { side } => {
            writeln!(out, "  {} snapshot unavailable.", side_name(*side))
        }
        DiffGap::Inspection { side, gap } => {
            write!(out, "  {} {:?}", side_name(*side), gap.field)?;
            match &gap.scope {
                InspectionGapScope::Target => (),
                InspectionGapScope::Schema { schema } => {
                    write!(out, " for schema {}", Identifier(schema))?
                }
                InspectionGapScope::Object { object } => write!(out, " for {}", Object(object))?,
                InspectionGapScope::Migration { .. } => (),
            }
            writeln!(out, " — {:?}.", gap.reason)
        }
        DiffGap::PresenceUnconfirmed {
            missing_from,
            object,
        } => writeln!(
            out,
            "  {} presence unconfirmed for {}.",
            side_name(*missing_from),
            Object(&object.identity)
        ),
        DiffGap::PartitionPresenceUnconfirmed { missing_from, edge } => writeln!(
            out,
            "  {} partition edge unconfirmed for {} under {}.",
            side_name(*missing_from),
            Qualified(&edge.child),
            Qualified(&edge.parent),
        ),
        DiffGap::DependencyPresenceUnconfirmed { missing_from, edge } => writeln!(
            out,
            "  {} dependency unconfirmed from {} to {}.",
            side_name(*missing_from),
            Object(&edge.dependent),
            Object(&edge.referenced),
        ),
        DiffGap::FieldUnavailable {
            side,
            object,
            field,
        } => writeln!(
            out,
            "  {} {field:?} unavailable for {}.",
            side_name(*side),
            Object(&object.identity)
        ),
    })
}

fn write_details<T, W: Write>(
    mut items: impl Iterator<Item = T>,
    out: &mut W,
    mut write: impl FnMut(&mut W, T) -> io::Result<()>,
) -> io::Result<()> {
    for item in items.by_ref().take(TEXT_DETAILS) {
        write(out, item)?;
    }
    let more = items.count();
    if more > 0 {
        writeln!(out, "  {more} more entries in JSON output.")?;
    }
    Ok(())
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

struct Identifier<'a>(&'a str);

impl fmt::Display for Identifier<'_> {
    fn fmt(&self, out: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(out, "\"{}\"", Terminal(&self.0.replace('"', "\"\"")))
    }
}

struct Qualified<'a>(&'a QualifiedName);

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
