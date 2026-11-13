//! Headless offline CLI for PostgreSQL migration review.
#![forbid(unsafe_code)]

mod config;
mod policy;
mod render;

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
use policy::ValidatedPolicy;
use render::{Terminal, write_file, write_json, write_report, write_text};

const INVALID_INPUT: u8 = 2;
const OPERATIONAL_FAILURE: u8 = 3;
const MAX_MANIFEST_BYTES: u64 = 8 * 1024 * 1024;
const MAX_SAVED_REPORT_BYTES: u64 = 256 * 1024 * 1024;

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
    },
    /// Render a saved versioned JSON report for review.
    Report {
        /// Saved JSON report.
        #[arg(long, value_name = "FILE")]
        input: PathBuf,

        /// Review output format.
        #[arg(long, value_enum)]
        format: ReviewFormat,

        /// Write the review atomically to a file instead of stdout.
        #[arg(long, value_name = "FILE")]
        output: Option<PathBuf>,
    },
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

#[derive(Clone, Copy, ValueEnum)]
enum ReviewFormat {
    Markdown,
    Github,
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
        Command::Check { analysis, policy } => {
            let policy = ValidatedPolicy::load(&policy)
                .map_err(|error| Failure::invalid(error.to_string()))?;
            run_analysis(analysis, Some(&policy))
        }
        Command::Report {
            input,
            format,
            output,
        } => render_saved_report(&input, format, output.as_deref()),
    }
}

fn render_saved_report(
    input: &Path,
    format: ReviewFormat,
    output: Option<&Path>,
) -> Result<u8, Failure> {
    let bytes = config::read_input(input, MAX_SAVED_REPORT_BYTES).map_err(|_| {
        Failure::operational("could not read the saved report", OPERATIONAL_FAILURE)
    })?;
    let input = std::str::from_utf8(&bytes)
        .map_err(|_| Failure::invalid("saved report must be UTF-8 JSON"))?;
    let report = ReportV1::from_json(input)
        .map_err(|_| Failure::invalid("invalid versioned report JSON"))?;
    write_report(output, |out| match format {
        ReviewFormat::Markdown => dmg_report::write_markdown(&report, out),
        ReviewFormat::Github => dmg_report::write_github_annotations(&report, out),
    })
    .map_err(|_| Failure::operational("could not write the review output", OPERATIONAL_FAILURE))?;
    Ok(0)
}

fn status_line(message: &str) -> Result<u8, Failure> {
    writeln!(io::stdout().lock(), "{message}")
        .map(|_| 0)
        .map_err(|_| Failure::operational("could not write command output", OPERATIONAL_FAILURE))
}

fn run_analysis(args: AnalysisArgs, policy: Option<&ValidatedPolicy>) -> Result<u8, Failure> {
    let project =
        Project::load(&args.config).map_err(|error| Failure::invalid(error.to_string()))?;
    let mut discovery = if let Some((base, head)) = args.base.as_deref().zip(args.head.as_deref()) {
        let revisions =
            GitRevisions::new(base, head).map_err(|error| Failure::invalid(error.to_string()))?;
        discover_git(&args.repo, &revisions, &project.discovery)
    } else {
        discover_worktree(&args.repo, &project.discovery)
    };
    load_evidence(&args, &mut discovery);

    let plan = parse_plan(&discovery.plan, project.target, |path| {
        discovery.source_bytes(path)
    });
    // parse_plan returns one entry for each forward in inventory order.
    for (file, parsed) in discovery
        .plan
        .inventory
        .iter_mut()
        .filter(|file| file.direction == MigrationDirection::Forward)
        .zip(&plan.migrations)
    {
        file.execution = parsed.execution;
    }
    let Analysis {
        coverage,
        mut findings,
        locks,
    } = analyze(&plan, project.target, &project.rules);
    let decision = policy.map(|policy| policy.evaluate(&discovery.plan, &coverage, &mut findings));
    let exit = analysis_exit(
        project.exit_codes,
        &coverage,
        decision.as_ref().map(|decision| decision.status),
    );
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
    .with_relation_locks(locks);
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

fn load_evidence(args: &AnalysisArgs, discovery: &mut Discovery) {
    let mut gaps = Vec::new();
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
