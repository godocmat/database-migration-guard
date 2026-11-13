//! Explicit live inspection and offline import of pinned native history.

use std::path::{Path, PathBuf};

use clap::Args;
use dmg_core::{
    coverage::{CoverageGap, CoverageGapKind},
    digest::ContentDigest,
    history::{NativeHistoryCoverage, NativeHistoryEvidenceV1, forward_inventory_digest},
    inspection::{
        InspectionEvidenceError, InspectionEvidenceV1, InspectionField, InspectionGapReason,
        InspectionGapScope, InspectionScope, SchemaState, TargetId,
    },
    snapshot::QualifiedName,
    target::PostgresTarget,
};
use dmg_discovery::{Discovery, reconcile_native_history};
use dmg_inspect::{
    CatalogDetails, CatalogOptions, HistoryOptions, InspectionTarget, InspectionTimeouts,
};
use serde::{Deserialize, Serialize};

use crate::{
    Failure, OPERATIONAL_FAILURE,
    config::{Project, read_input, toml_location},
    discover_project, evidence_gap, parse_digest,
    render::write_report,
};

const MAX_CONNECTION_BYTES: u64 = 64 * 1024;
const MAX_CA_BYTES: u64 = 256 * 1024;
const MAX_PASSWORD_BYTES: u64 = 4096;
const MAX_HISTORY_BYTES: u64 = 32 * 1024 * 1024;

#[derive(Args)]
pub(crate) struct InspectArgs {
    /// Protected PostgreSQL connection TOML.
    #[arg(long, value_name = "FILE")]
    connection: PathBuf,

    /// Write the evidence atomically to a file instead of stdout.
    #[arg(long, value_name = "FILE")]
    output: Option<PathBuf>,

    /// Read the designated native history table for a discovered migration inventory.
    #[arg(long, requires = "config")]
    history: bool,

    /// Project configuration TOML for history discovery.
    #[arg(long, value_name = "FILE", requires = "history")]
    config: Option<PathBuf>,

    /// Repository root or working directory for history discovery (default ".").
    #[arg(long, value_name = "DIR", requires = "history")]
    repo: Option<PathBuf>,

    /// Full local Git commit ID for the trusted base.
    #[arg(long, value_name = "COMMIT", requires_all = ["history", "head"])]
    base: Option<String>,

    /// Full local Git commit ID for the candidate.
    #[arg(long, value_name = "COMMIT", requires_all = ["history", "base"])]
    head: Option<String>,
}

#[derive(Args)]
pub(crate) struct NativeHistoryArgs {
    /// Trusted native history JSON from dmg inspect --history.
    #[arg(long, value_name = "FILE", requires_all = ["native_history_sha256", "target_id"], conflicts_with_all = ["applied_manifest", "applied_sha256"])]
    native_history: Option<PathBuf>,

    /// SHA-256 of the exact saved native history file bytes.
    #[arg(long, value_name = "SHA256", requires = "native_history", value_parser = parse_digest)]
    native_history_sha256: Option<ContentDigest>,

    /// Expected opaque target ID for the saved native history.
    #[arg(long, value_name = "ID", requires = "native_history", value_parser = parse_target_id)]
    target_id: Option<TargetId>,
}

fn parse_target_id(value: &str) -> Result<TargetId, InspectionEvidenceError> {
    TargetId::new(value)
}

impl NativeHistoryArgs {
    pub(crate) fn target_id(&self) -> Option<&TargetId> {
        self.target_id.as_ref()
    }

    pub(crate) fn load(
        &self,
        discovery: &mut Discovery,
        target: PostgresTarget,
    ) -> Vec<CoverageGap> {
        let (Some(path), Some(digest), Some(target_id)) = (
            self.native_history.as_deref(),
            self.native_history_sha256,
            self.target_id.as_ref(),
        ) else {
            return Vec::new();
        };
        match import_history(path, digest, target_id, target, discovery) {
            Ok(result) => {
                discovery.plan.applied_history = Some(result.applied_history);
                result.coverage.gaps().to_vec()
            }
            Err(message) => vec![evidence_gap(
                CoverageGapKind::MissingAppliedHistory,
                "Native history evidence",
                message,
            )],
        }
    }
}

fn import_history(
    path: &Path,
    digest: ContentDigest,
    target_id: &TargetId,
    target: PostgresTarget,
    discovery: &Discovery,
) -> Result<dmg_discovery::NativeHistoryReconciliation, String> {
    let bytes = read_input(path, MAX_HISTORY_BYTES).map_err(|error| error.to_string())?;
    if ContentDigest::of(&bytes) != digest {
        return Err("the saved file SHA-256 does not match its trusted pin".into());
    }
    let evidence: NativeHistoryEvidenceV1 =
        serde_json::from_slice(&bytes).map_err(|_| "invalid versioned native history JSON")?;
    if evidence
        .observation()
        .server_version_num
        .is_some_and(|version| version / 10_000 != u32::from(target.major()))
    {
        return Err("the observed server major differs from the project target".into());
    }
    reconcile_native_history(discovery, target_id, &evidence).map_err(|error| error.to_string())
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ConnectionFile {
    schema_version: u32,
    target_id: TargetId,
    target: PostgresTarget,
    schemas: InspectionScope,
    host: String,
    port: u16,
    database: String,
    user: String,
    ca_file: PathBuf,
    password_file: PathBuf,
    #[serde(default)]
    definitions: bool,
    #[serde(default)]
    total_bytes: bool,
    row_limit: Option<u16>,
    #[serde(default)]
    timeouts: TimeoutsFile,
    history: Option<HistoryFile>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct HistoryFile {
    schema: String,
    table: String,
    row_limit: Option<u16>,
}

#[derive(Deserialize)]
#[serde(default, deny_unknown_fields)]
struct TimeoutsFile {
    overall_ms: u32,
    statement_ms: u32,
    lock_ms: u32,
    idle_transaction_ms: u32,
}

impl Default for TimeoutsFile {
    fn default() -> Self {
        Self {
            overall_ms: 5000,
            statement_ms: 1000,
            lock_ms: 100,
            idle_transaction_ms: 3000,
        }
    }
}

impl ConnectionFile {
    fn load(path: &Path) -> Result<Self, Failure> {
        let bytes = read_input(path, MAX_CONNECTION_BYTES)
            .map_err(|error| Failure::invalid(format!("connection configuration: {error}")))?;
        let input = std::str::from_utf8(&bytes)
            .map_err(|_| Failure::invalid("connection configuration must be UTF-8"))?;
        let file: Self = toml::from_str(input).map_err(|error| {
            Failure::invalid(format!(
                "invalid connection configuration TOML{}",
                toml_location(input, &error),
            ))
        })?;
        if file.schema_version != 1 {
            return Err(Failure::invalid(
                "connection configuration requires schema_version = 1",
            ));
        }
        Ok(file)
    }

    fn connect_target(&self, path: &Path) -> Result<InspectionTarget, Failure> {
        let directory = path.parent().unwrap_or(Path::new("."));
        let ca = read_input(&directory.join(&self.ca_file), MAX_CA_BYTES)
            .map_err(|error| Failure::invalid(format!("inspection CA file: {error}")))?;
        let password = read_input(&directory.join(&self.password_file), MAX_PASSWORD_BYTES)
            .map_err(|error| Failure::invalid(format!("inspection password file: {error}")))?;
        InspectionTarget::new(
            &self.host,
            self.port,
            &self.database,
            &self.user,
            password,
            &ca,
        )
        .map_err(|error| Failure::invalid(error.to_string()))
    }

    fn timeouts(&self) -> Result<InspectionTimeouts, Failure> {
        InspectionTimeouts::new(
            self.timeouts.overall_ms,
            self.timeouts.statement_ms,
            self.timeouts.lock_ms,
            self.timeouts.idle_transaction_ms,
        )
        .map_err(|error| Failure::invalid(error.to_string()))
    }

    fn catalog_options(&self) -> Result<CatalogOptions, Failure> {
        let mut options = CatalogOptions::new(u32::from(self.target.major()))
            .map_err(|error| Failure::invalid(error.to_string()))?
            .with_details(CatalogDetails {
                definitions: self.definitions,
                total_bytes: self.total_bytes,
            });
        if let Some(limit) = self.row_limit {
            options = options
                .with_row_limit(limit)
                .map_err(|error| Failure::invalid(error.to_string()))?;
        }
        Ok(options)
    }
}

pub(crate) fn run(args: InspectArgs) -> Result<u8, Failure> {
    let file = ConnectionFile::load(&args.connection)?;
    let timeouts = file.timeouts()?;
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|_| {
            Failure::operational(
                "could not start the inspection runtime",
                OPERATIONAL_FAILURE,
            )
        })?;

    if args.history {
        let project_path = args
            .config
            .as_deref()
            .ok_or_else(|| Failure::invalid("native history requires a project configuration"))?;
        let (inventory, options) = history_options(&file, &args, project_path)?;
        let target = file.connect_target(&args.connection)?;
        let evidence = runtime
            .block_on(target.inspect_history(file.target_id, inventory, &options, timeouts))
            .map_err(|_| {
                Failure::operational(
                    "could not produce native history evidence",
                    OPERATIONAL_FAILURE,
                )
            })?;
        let complete =
            evidence.coverage() == NativeHistoryCoverage::Complete && evidence.gaps().is_empty();
        save_evidence(args.output.as_deref(), &evidence, complete)
    } else {
        let options = file.catalog_options()?;
        let target = file.connect_target(&args.connection)?;
        let evidence = runtime
            .block_on(target.inspect_catalog(file.target_id, &file.schemas, options, timeouts))
            .map_err(|_| {
                Failure::operational("could not produce catalog evidence", OPERATIONAL_FAILURE)
            })?;
        save_evidence(
            args.output.as_deref(),
            &evidence,
            catalog_complete(&evidence),
        )
    }
}

fn history_options(
    file: &ConnectionFile,
    args: &InspectArgs,
    project_path: &Path,
) -> Result<(ContentDigest, HistoryOptions), Failure> {
    let source = file.history.as_ref().ok_or_else(|| {
        Failure::invalid(
            "native history requires a [history] source in the connection configuration",
        )
    })?;
    let project =
        Project::load(project_path).map_err(|error| Failure::invalid(error.to_string()))?;
    if file.target != project.target {
        return Err(Failure::invalid(
            "the connection target major differs from the project target",
        ));
    }
    let repo = args.repo.as_deref().unwrap_or(Path::new("."));
    let revisions = args.base.as_deref().zip(args.head.as_deref());
    let discovery = discover_project(repo, revisions, &project)?;
    if !discovery.head_complete() {
        return Err(Failure::operational(
            "could not discover a complete candidate inventory for native history",
            OPERATIONAL_FAILURE,
        ));
    }
    let mut options = HistoryOptions::new(
        u32::from(file.target.major()),
        discovery.adapter(),
        QualifiedName {
            schema: source.schema.clone(),
            name: source.table.clone(),
        },
    )
    .map_err(|error| Failure::invalid(error.to_string()))?;
    if let Some(limit) = source.row_limit {
        options = options
            .with_row_limit(limit)
            .map_err(|error| Failure::invalid(error.to_string()))?;
    }
    let inventory = forward_inventory_digest(discovery.adapter(), &discovery.plan.inventory)
        .map_err(|_| {
            Failure::operational(
                "could not bind the candidate inventory for native history",
                OPERATIONAL_FAILURE,
            )
        })?;
    Ok((inventory, options))
}

fn catalog_complete(evidence: &InspectionEvidenceV1) -> bool {
    evidence
        .scope()
        .iter()
        .all(|schema| matches!(schema.state, SchemaState::Visible | SchemaState::Missing))
        && evidence
            .gaps()
            .iter()
            .chain(
                evidence
                    .snapshot()
                    .into_iter()
                    .flat_map(|snapshot| snapshot.gaps()),
            )
            .all(|gap| match (&gap.scope, gap.field, gap.reason) {
                (
                    InspectionGapScope::Schema { schema },
                    InspectionField::Scope,
                    InspectionGapReason::Missing,
                ) => evidence.scope().iter().any(|observation| {
                    observation.schema == *schema && observation.state == SchemaState::Missing
                }),
                (_, InspectionField::PlannerRows | InspectionField::EstimatedHeapBytes, _) => true,
                _ => false,
            })
}

fn save_evidence(
    output: Option<&Path>,
    evidence: &impl Serialize,
    complete: bool,
) -> Result<u8, Failure> {
    write_report(output, |out| {
        serde_json::to_writer_pretty(&mut *out, evidence)?;
        writeln!(out)
    })
    .map_err(|_| {
        Failure::operational("could not write inspection evidence", OPERATIONAL_FAILURE)
    })?;
    Ok(if complete { 0 } else { OPERATIONAL_FAILURE })
}
