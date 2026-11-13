//! Headless CLI for PostgreSQL migration review and opt-in inspection.
#![forbid(unsafe_code)]

mod config;
mod diff;
mod inspection;
mod policy;
mod render;
mod review;
mod simulate;

use std::{
    io::{self, Write},
    path::{Path, PathBuf},
    process::ExitCode,
};

use clap::{Args, Parser, Subcommand, ValueEnum};
use config::{ExitCodes, Project};
use dmg_core::{
    coverage::{Coverage, CoverageGap, CoverageGapKind, CoverageStatus, DiagnosticScope},
    digest::ContentDigest,
    migration::MigrationDirection,
    policy::DecisionStatus,
    report::{ReportMetadata, ReportV1},
    target::PostgresTarget,
};
use dmg_discovery::{
    Discovery, GitRevisions, compare_integrity_manifest, discover_git, discover_worktree,
    reconcile_applied_manifest,
};
use dmg_postgres::parse_plan;
use dmg_rules::{Analysis, RULESET_VERSION, analyze};
use policy::{PolicyAnalysis, PolicyEvidenceArgs, ValidatedPolicy};
use render::{Terminal, write_file, write_json, write_report, write_text};

const INVALID_INPUT: u8 = 2;
const OPERATIONAL_FAILURE: u8 = 3;
const MAX_MANIFEST_BYTES: u64 = 8 * 1024 * 1024;

#[derive(Parser)]
#[command(name = "dmg", version, about, color = clap::ColorChoice::Never)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Write a starter project configuration.
    Init {
        /// PostgreSQL target major.
        #[arg(long, value_parser = parse_target)]
        target: PostgresTarget,

        /// Destination project configuration.
        #[arg(long, value_name = "FILE", default_value = "dmg.toml")]
        config: PathBuf,
    },
    /// Validate a versioned policy.
    Policy {
        #[command(subcommand)]
        command: PolicyCommand,
    },
    /// Analyze migrations and report findings.
    Scan(AnalysisArgs),
    /// Analyze migrations and apply a trusted policy.
    Check {
        #[command(flatten)]
        analysis: AnalysisArgs,

        /// Trusted policy TOML supplied by the caller.
        #[arg(long, value_name = "FILE")]
        policy: PathBuf,

        #[command(flatten)]
        evidence: PolicyEvidenceArgs,
    },
    /// Inspect authorized PostgreSQL catalogs or a native history table over verified TLS.
    Inspect(inspection::InspectArgs),
    /// Compare saved PostgreSQL schemas and application contracts.
    Diff(diff::DiffArgs),
    /// Rehearse a reviewed Git revision on a disposable PostgreSQL worker.
    Simulate(simulate::SimulateArgs),
    /// Render a saved versioned JSON report for review.
    Report(review::ReviewArgs),
}

#[derive(Subcommand)]
enum PolicyCommand {
    /// Validate policy syntax, version, and rule IDs.
    Validate {
        /// Policy TOML file.
        #[arg(long, value_name = "FILE")]
        policy: PathBuf,
    },
}

#[derive(Args)]
struct AnalysisArgs {
    /// Repository root or working directory containing the migration roots.
    #[arg(long, value_name = "DIR", default_value = ".")]
    repo: PathBuf,

    /// Project configuration TOML.
    #[arg(long, value_name = "FILE", default_value = "dmg.toml")]
    config: PathBuf,

    /// Full local Git commit ID for the trusted base.
    #[arg(long, value_name = "COMMIT", requires = "head")]
    base: Option<String>,

    /// Full local Git commit ID for the candidate.
    #[arg(long, value_name = "COMMIT", requires = "base")]
    head: Option<String>,

    /// Trusted file integrity manifest for a local worktree scan.
    #[arg(long, value_name = "FILE", requires = "integrity_sha256", conflicts_with_all = ["base", "head"])]
    integrity_manifest: Option<PathBuf>,

    /// SHA-256 of the trusted file integrity manifest.
    #[arg(long, value_name = "SHA256", requires = "integrity_manifest", value_parser = parse_digest)]
    integrity_sha256: Option<ContentDigest>,

    /// Trusted applied-history manifest.
    #[arg(long, value_name = "FILE", requires = "applied_sha256")]
    applied_manifest: Option<PathBuf>,

    /// SHA-256 of the trusted applied-history manifest.
    #[arg(long, value_name = "SHA256", requires = "applied_manifest", value_parser = parse_digest)]
    applied_sha256: Option<ContentDigest>,

    #[command(flatten)]
    native_history: inspection::NativeHistoryArgs,

    /// Report output format.
    #[arg(long, value_enum, default_value = "text")]
    format: ReportFormat,

    /// Write the report atomically to a file instead of stdout.
    #[arg(long, value_name = "FILE")]
    output: Option<PathBuf>,
}

#[derive(Clone, Copy, ValueEnum)]
enum ReportFormat {
    Text,
    Json,
}

fn parse_target(value: &str) -> Result<PostgresTarget, &'static str> {
    match value {
        "15" => Ok(PostgresTarget::V15),
        "16" => Ok(PostgresTarget::V16),
        "17" => Ok(PostgresTarget::V17),
        "18" => Ok(PostgresTarget::V18),
        _ => Err("expected PostgreSQL major 15 through 18"),
    }
}

fn parse_digest(value: &str) -> Result<ContentDigest, dmg_core::digest::InvalidContentDigest> {
    value.parse()
}

fn main() -> ExitCode {
    let code = match Cli::try_parse() {
        Ok(cli) => match run(cli.command) {
            Ok(code) => code,
            Err(failure) => {
                let _ = writeln!(io::stderr().lock(), "dmg: {}", Terminal(&failure.message));
                failure.code
            }
        },
        Err(error) if !error.use_stderr() => match error.print() {
            Ok(()) => 0,
            Err(_) => OPERATIONAL_FAILURE,
        },
        Err(error) => {
            let _ = writeln!(
                io::stderr().lock(),
                "dmg: invalid invocation ({:?}). See dmg --help.",
                error.kind()
            );
            INVALID_INPUT
        }
    };
    ExitCode::from(code)
}

fn run(command: Command) -> Result<u8, Failure> {
    match command {
        Command::Init { target, config } => {
            let contents = config::template(target);
            write_file(&config, false, |out| out.write_all(contents.as_bytes())).map_err(
                |error| {
                    if error.kind() == io::ErrorKind::AlreadyExists {
                        Failure::invalid("the project configuration already exists")
                    } else {
                        Failure::operational(
                            "could not create the project configuration",
                            OPERATIONAL_FAILURE,
                        )
                    }
                },
            )?;
            status_line("Project configuration created.")
        }
        Command::Policy {
            command: PolicyCommand::Validate { policy },
        } => {
            ValidatedPolicy::load(&policy).map_err(|error| Failure::invalid(error.to_string()))?;
            status_line("Policy is valid.")
        }
        Command::Scan(analysis) => run_analysis(analysis, None),
        Command::Check {
            analysis,
            policy,
            evidence,
        } => {
            let policy = ValidatedPolicy::load(&policy)
                .map_err(|error| Failure::invalid(error.to_string()))?;
            run_analysis(analysis, Some((&policy, &evidence)))
        }
        Command::Inspect(args) => inspection::run(args),
        Command::Diff(args) => diff::run(args),
        Command::Simulate(args) => simulate::run(args),
        Command::Report(args) => review::run(args),
    }
}

fn status_line(message: &str) -> Result<u8, Failure> {
    writeln!(io::stdout().lock(), "{message}")
        .map(|_| 0)
        .map_err(|_| Failure::operational("could not write command output", OPERATIONAL_FAILURE))
}

fn run_analysis(
    args: AnalysisArgs,
    policy: Option<(&ValidatedPolicy, &PolicyEvidenceArgs)>,
) -> Result<u8, Failure> {
    let project =
        Project::load(&args.config).map_err(|error| Failure::invalid(error.to_string()))?;
    let revisions = args.base.as_deref().zip(args.head.as_deref());
    let mut discovery = discover_project(&args.repo, revisions, &project)?;
    load_evidence(&args, &mut discovery, project.target);
    if let Some(((policy, _), target_id)) = policy.zip(args.native_history.target_id())
        && policy
            .target_id()
            .is_some_and(|expected| expected != target_id)
    {
        let mut gaps = discovery.plan.coverage.gaps().to_vec();
        gaps.push(evidence_gap(
            CoverageGapKind::MissingAppliedHistory,
            "Native history evidence",
            "the opaque target ID differs from the protected policy".into(),
        ));
        discovery.plan.coverage = Coverage::from_gaps(gaps);
        discovery.plan.applied_history = None;
    }

    let plan = parse_plan(&discovery.plan, project.target, |path| {
        discovery.source_bytes(path)
    });
    let Analysis {
        coverage,
        mut findings,
        locks,
    } = analyze(&plan, project.target, &project.rules);
    let (decision, audit) = policy.map_or((None, None), |(policy, evidence)| {
        let evaluated = policy.evaluate(
            evidence,
            PolicyAnalysis {
                repository: &args.repo,
                discovery: &discovery,
                project: &project,
                coverage: &coverage,
            },
            &mut findings,
        );
        (Some(evaluated.decision), evaluated.audit)
    });
    let exit = analysis_exit(
        project.exit_codes,
        &coverage,
        decision.as_ref().map(|decision| decision.status),
    );
    // Keep the runner declarations available to rehearsal planning before reporting effective modes.
    for (file, parsed) in discovery
        .plan
        .inventory
        .iter_mut()
        .filter(|file| file.direction == MigrationDirection::Forward)
        .zip(&plan.migrations)
    {
        file.execution = parsed.execution;
    }
    let report = ReportV1::new(
        ReportMetadata {
            tool_version: env!("CARGO_PKG_VERSION").into(),
            ruleset_version: RULESET_VERSION.into(),
            target: project.target,
        },
        discovery.plan,
        coverage.gaps().iter().cloned(),
        findings,
        decision,
    )
    .with_relation_locks(locks)
    .with_policy_audit(audit);
    write_report(args.output.as_deref(), |out| match args.format {
        ReportFormat::Text => write_text(&report, out),
        ReportFormat::Json => write_json(&report, out),
    })
    .map_err(|_| {
        Failure::operational("could not write the report", project.exit_codes.incomplete)
    })?;
    Ok(exit)
}

fn analysis_exit(codes: ExitCodes, coverage: &Coverage, decision: Option<DecisionStatus>) -> u8 {
    match decision {
        Some(DecisionStatus::Incomplete) => codes.incomplete,
        Some(DecisionStatus::Denied) => codes.denied,
        Some(DecisionStatus::Pass) => 0,
        None if coverage.status() == CoverageStatus::Incomplete => codes.incomplete,
        None => 0,
    }
}

fn discover_project(
    repository: &Path,
    revisions: Option<(&str, &str)>,
    project: &Project,
) -> Result<Discovery, Failure> {
    Ok(if let Some((base, head)) = revisions {
        let revisions =
            GitRevisions::new(base, head).map_err(|error| Failure::invalid(error.to_string()))?;
        discover_git(repository, &revisions, &project.discovery)
    } else {
        discover_worktree(repository, &project.discovery)
    })
}

fn load_evidence(args: &AnalysisArgs, discovery: &mut Discovery, target: PostgresTarget) {
    let mut gaps = args.native_history.load(discovery, target);
    if let Some((path, digest)) = args
        .integrity_manifest
        .as_deref()
        .zip(args.integrity_sha256)
        && let Err(message) = apply_manifest(path, digest, discovery, compare_integrity_manifest)
    {
        gaps.push(evidence_gap(
            CoverageGapKind::MissingIntegrityEvidence,
            "Integrity manifest",
            message,
        ));
    }
    if let Some((path, digest)) = args.applied_manifest.as_deref().zip(args.applied_sha256)
        && let Err(message) = apply_manifest(path, digest, discovery, reconcile_applied_manifest)
    {
        gaps.push(evidence_gap(
            CoverageGapKind::MissingAppliedHistory,
            "Applied-history manifest",
            message,
        ));
    }
    if !gaps.is_empty() {
        gaps.extend_from_slice(discovery.plan.coverage.gaps());
        discovery.plan.coverage = Coverage::from_gaps(gaps);
    }
}

fn apply_manifest(
    path: &Path,
    digest: ContentDigest,
    discovery: &mut Discovery,
    apply: impl FnOnce(&mut Discovery, &[u8], ContentDigest) -> Result<(), dmg_discovery::ManifestError>,
) -> Result<(), String> {
    let bytes = config::read_input(path, MAX_MANIFEST_BYTES).map_err(|error| error.to_string())?;
    apply(discovery, &bytes, digest).map_err(|error| error.to_string())
}

fn evidence_gap(kind: CoverageGapKind, name: &str, message: String) -> CoverageGap {
    CoverageGap {
        scope: DiagnosticScope::Repository,
        kind,
        message: format!("{name} — {message}"),
        related_paths: Vec::new(),
    }
}

struct Failure {
    code: u8,
    message: String,
}

impl Failure {
    fn invalid(message: impl Into<String>) -> Self {
        Self {
            code: INVALID_INPUT,
            message: message.into(),
        }
    }

    fn operational(message: impl Into<String>, code: u8) -> Self {
        Self {
            code,
            message: message.into(),
        }
    }
}
