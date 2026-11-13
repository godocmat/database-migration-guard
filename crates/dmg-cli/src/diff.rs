use std::{
    fmt,
    path::{Path, PathBuf},
};

use clap::Args;
use dmg_core::{
    application::{ApplicationContractV1, MAX_APPLICATION_CONTRACT_BYTES, evaluate_contract},
    coverage::CoverageStatus,
    diff::{DiffError, RenameMapping, SchemaInput, compare_schemas},
    inspection::InspectionEvidenceV1,
    report::DiffReportV1,
    snapshot::SchemaSnapshotV1,
};
use serde::{
    Deserialize, Deserializer,
    de::{self, IgnoredAny, MapAccess, Visitor},
};

use crate::{
    Failure, OPERATIONAL_FAILURE, ReportFormat,
    config::{InputError, read_input},
    render::write_report,
};

const MAX_SCHEMA_BYTES: u64 = 32 * 1024 * 1024;
const MAX_RENAME_BYTES: u64 = 1024 * 1024;
const MAX_RENAMES: usize = 4096;
const MAX_CONTRACTS: usize = 16;

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

    let report = DiffReportV1::new(comparison, application_contracts);
    let complete = report.coverage() == CoverageStatus::Complete;

    write_report(args.output.as_deref(), |mut out| match args.format {
        ReportFormat::Text => dmg_report::write_diff_text(&report, out),
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
