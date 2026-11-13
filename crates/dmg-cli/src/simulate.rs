//! Explicit, revision-bound rehearsal on a disposable PostgreSQL worker.

use std::path::{Path, PathBuf};

use clap::Args;
use dmg_core::{
    coverage::Coverage,
    inspection::{InspectionScope, TargetId},
};
use dmg_simulate::{
    DockerWorkerConfig, MAX_REHEARSAL_SETUP_BYTES, PostgresImage, RehearsalContext, RehearsalInput,
    RehearsalOutcome, RehearsalPlanError, SeedKind, WorkerLimits, prepare_rehearsal, run_rehearsal,
};
use serde::{Deserialize, Serialize};

use crate::{
    Failure, OPERATIONAL_FAILURE,
    config::{InputError, Project, read_input, toml_location},
    discover_project,
    policy::ValidatedPolicy,
    render::write_report,
};

const MAX_REHEARSAL_CONFIG_BYTES: u64 = 64 * 1024;

#[derive(Args)]
pub(crate) struct SimulateArgs {
    /// Local Git repository containing the exact revisions.
    #[arg(long, value_name = "DIR", default_value = ".")]
    repo: PathBuf,

    /// Full local Git commit ID for the trusted base.
    #[arg(long, value_name = "COMMIT")]
    base: String,

    /// Full local Git commit ID for the reviewed candidate.
    #[arg(long, value_name = "COMMIT")]
    head: String,

    /// Trusted project configuration TOML.
    #[arg(long, value_name = "FILE")]
    config: PathBuf,

    /// Trusted policy TOML.
    #[arg(long, value_name = "FILE")]
    policy: PathBuf,

    /// Protected rehearsal inputs and worker settings.
    #[arg(long, value_name = "FILE")]
    rehearsal_config: PathBuf,

    /// Print the expected binding without starting a worker.
    #[arg(long)]
    plan_only: bool,

    /// Write the JSON atomically to a file instead of stdout.
    #[arg(long, value_name = "FILE")]
    output: Option<PathBuf>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RehearsalFile {
    schema_version: u32,
    repository_id: TargetId,
    target_id: TargetId,
    schemas: InspectionScope,
    postgres: ApprovedImage,
    setup: SetupFiles,
    worker: Option<WorkerFile>,
}

#[derive(Deserialize)]
enum ApprovedImage {
    #[serde(rename = "15.20")]
    V15_20,
    #[serde(rename = "16.16")]
    V16_16,
    #[serde(rename = "17.12")]
    V17_12,
    #[serde(rename = "18.7")]
    V18_7,
}

impl From<ApprovedImage> for PostgresImage {
    fn from(image: ApprovedImage) -> Self {
        match image {
            ApprovedImage::V15_20 => Self::V15_20,
            ApprovedImage::V16_16 => Self::V16_16,
            ApprovedImage::V17_12 => Self::V17_12,
            ApprovedImage::V18_7 => Self::V18_7,
        }
    }
}

#[derive(Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
enum SetupFiles {
    EmptyDatabase {},
    SchemaOnly {
        baseline_file: PathBuf,
    },
    Seeded {
        baseline_file: PathBuf,
        seed_file: PathBuf,
        seed_kind: SeedKind,
    },
}

enum Setup {
    EmptyDatabase,
    SchemaOnly {
        baseline_sql: Vec<u8>,
    },
    Seeded {
        baseline_sql: Vec<u8>,
        seed_sql: Vec<u8>,
        seed_kind: SeedKind,
    },
}

impl SetupFiles {
    fn load(self, directory: &Path) -> Result<Setup, Failure> {
        match self {
            Self::EmptyDatabase {} => Ok(Setup::EmptyDatabase),
            Self::SchemaOnly { baseline_file } => Ok(Setup::SchemaOnly {
                baseline_sql: read_script(directory, &baseline_file, "baseline")?,
            }),
            Self::Seeded {
                baseline_file,
                seed_file,
                seed_kind,
            } => Ok(Setup::Seeded {
                baseline_sql: read_script(directory, &baseline_file, "baseline")?,
                seed_sql: read_script(directory, &seed_file, "seed")?,
                seed_kind,
            }),
        }
    }
}

impl Setup {
    fn as_input(&self) -> RehearsalInput<'_> {
        match self {
            Self::EmptyDatabase => RehearsalInput::EmptyDatabase,
            Self::SchemaOnly { baseline_sql } => RehearsalInput::SchemaOnly { baseline_sql },
            Self::Seeded {
                baseline_sql,
                seed_sql,
                seed_kind,
            } => RehearsalInput::Seeded {
                baseline_sql,
                seed_sql,
                seed_kind: *seed_kind,
            },
        }
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct WorkerFile {
    docker_executable: PathBuf,
    docker_socket: PathBuf,
    timeouts: Option<Timeouts>,
}

#[derive(Deserialize)]
#[serde(default, deny_unknown_fields)]
struct Timeouts {
    startup_ms: u32,
    operation_ms: u32,
    statement_ms: u32,
    lock_ms: u32,
    cleanup_ms: u32,
}

impl Default for Timeouts {
    fn default() -> Self {
        Self {
            startup_ms: 30_000,
            operation_ms: 60_000,
            statement_ms: 10_000,
            lock_ms: 1_000,
            cleanup_ms: 10_000,
        }
    }
}

impl WorkerFile {
    fn configure(self, image: PostgresImage) -> Result<DockerWorkerConfig, Failure> {
        let limits = self.timeouts.map_or_else(
            || Ok(WorkerLimits::default()),
            |timeouts| {
                WorkerLimits::new(
                    timeouts.startup_ms,
                    timeouts.operation_ms,
                    timeouts.statement_ms,
                    timeouts.lock_ms,
                    timeouts.cleanup_ms,
                )
            },
        );
        DockerWorkerConfig::new(
            self.docker_executable,
            self.docker_socket,
            image,
            limits.map_err(|error| Failure::invalid(error.to_string()))?,
        )
        .map_err(|error| Failure::invalid(error.to_string()))
    }
}

fn read_script(directory: &Path, path: &Path, label: &str) -> Result<Vec<u8>, Failure> {
    read_input(&directory.join(path), MAX_REHEARSAL_SETUP_BYTES as u64).map_err(|error| match error
    {
        InputError::Read(kind) => Failure::operational(
            format!("could not read the rehearsal {label} ({kind})"),
            OPERATIONAL_FAILURE,
        ),
        InputError::NotFile => {
            Failure::invalid(format!("rehearsal {label} must be a regular file"))
        }
        InputError::Limit => Failure::invalid(format!(
            "rehearsal {label} exceeds the {MAX_REHEARSAL_SETUP_BYTES}-byte limit"
        )),
    })
}

impl RehearsalFile {
    fn load(path: &Path) -> Result<Self, Failure> {
        let bytes = read_input(path, MAX_REHEARSAL_CONFIG_BYTES)
            .map_err(|error| Failure::invalid(format!("rehearsal configuration {error}")))?;
        let input = std::str::from_utf8(&bytes)
            .map_err(|_| Failure::invalid("rehearsal configuration must be UTF-8"))?;
        let file: Self = toml::from_str(input).map_err(|error| {
            Failure::invalid(format!(
                "invalid rehearsal configuration TOML{}",
                toml_location(input, &error),
            ))
        })?;
        if file.schema_version != 1 {
            return Err(Failure::invalid(
                "rehearsal configuration requires schema_version = 1",
            ));
        }
        Ok(file)
    }
}

#[derive(Serialize)]
struct IncompletePlan<'a> {
    schema_version: u32,
    outcome: RehearsalOutcome,
    coverage: &'a Coverage,
}

pub(crate) fn run(args: SimulateArgs) -> Result<u8, Failure> {
    let project =
        Project::load(&args.config).map_err(|error| Failure::invalid(error.to_string()))?;
    let policy =
        ValidatedPolicy::load(&args.policy).map_err(|error| Failure::invalid(error.to_string()))?;
    let file = RehearsalFile::load(&args.rehearsal_config)?;
    if policy.evidence_scope().is_some_and(|scope| {
        scope.repository_id != file.repository_id || scope.target_id != file.target_id
    }) {
        return Err(Failure::invalid(
            "rehearsal repository or target ID differs from the protected policy",
        ));
    }

    let image = file.postgres.into();
    let worker = file
        .worker
        .map(|worker| worker.configure(image))
        .transpose()?;
    if !args.plan_only && worker.is_none() {
        return Err(Failure::invalid(
            "rehearsal execution requires a protected worker table",
        ));
    }

    let discovery = discover_project(&args.repo, Some((&args.base, &args.head)), &project)?;
    let directory = args.rehearsal_config.parent().unwrap_or(Path::new("."));
    let setup = file.setup.load(directory)?;
    let context = RehearsalContext {
        repository_id: file.repository_id,
        target: project.target,
        target_id: file.target_id,
        scope: file.schemas,
        search_path: project.rules.search_path,
        project_config_digest: project.digest,
        policy_digest: policy.digest(),
    };
    let output = args.output.as_deref();
    let plan = match prepare_rehearsal(&discovery, context, setup.as_input(), image) {
        Ok(plan) => plan,
        Err(
            error @ (RehearsalPlanError::TargetMismatch | RehearsalPlanError::InvalidSearchPath),
        ) => {
            return Err(Failure::invalid(error.to_string()));
        }
        Err(RehearsalPlanError::Incomplete(coverage)) => {
            save_json(
                output,
                &IncompletePlan {
                    schema_version: 1,
                    outcome: RehearsalOutcome::Incomplete,
                    coverage: &coverage,
                },
            )?;
            return Ok(OPERATIONAL_FAILURE);
        }
        Err(error @ RehearsalPlanError::Binding) => {
            return Err(Failure::operational(error.to_string(), OPERATIONAL_FAILURE));
        }
    };

    if args.plan_only {
        save_json(output, plan.binding())?;
        return Ok(0);
    }
    let worker = worker
        .ok_or_else(|| Failure::invalid("rehearsal execution requires a protected worker table"))?;
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|_| {
            Failure::operational("could not start the rehearsal runtime", OPERATIONAL_FAILURE)
        })?;
    let report = runtime.block_on(run_rehearsal(worker, plan));
    report.validate().map_err(|_| {
        Failure::operational(
            "the rehearsal produced invalid evidence",
            OPERATIONAL_FAILURE,
        )
    })?;
    save_json(output, &report)?;
    Ok(if report.outcome == RehearsalOutcome::Succeeded {
        0
    } else {
        OPERATIONAL_FAILURE
    })
}

fn save_json(output: Option<&Path>, document: &impl Serialize) -> Result<(), Failure> {
    write_report(output, |out| {
        serde_json::to_writer_pretty(&mut *out, document)?;
        writeln!(out)
    })
    .map_err(|_| Failure::operational("could not write the rehearsal output", OPERATIONAL_FAILURE))
}
