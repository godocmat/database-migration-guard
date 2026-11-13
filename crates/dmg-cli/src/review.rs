use std::{io, path::PathBuf};

use clap::{Args, ValueEnum};
use dmg_core::{
    rehearsal::RehearsalReportV1,
    report::{DiffReportV1, ReportV1},
};
use dmg_report::EvidenceFormat;

use crate::{Failure, OPERATIONAL_FAILURE, config::read_input, render::write_report};

const MAX_SAVED_REPORT_BYTES: u64 = 256 * 1024 * 1024;

#[derive(Args)]
pub(crate) struct ReviewArgs {
    /// Saved JSON report.
    #[arg(long, value_name = "FILE")]
    input: PathBuf,

    /// Type of saved report.
    #[arg(long, value_enum, default_value = "analysis")]
    kind: ReviewKind,

    /// Review output format. Diff and rehearsal reports support Markdown and HTML.
    #[arg(long, value_enum)]
    format: ReviewFormat,

    /// Write the review atomically to a file instead of stdout.
    #[arg(long, value_name = "FILE")]
    output: Option<PathBuf>,
}

#[derive(Clone, Copy, ValueEnum)]
enum ReviewKind {
    Analysis,
    Diff,
    Rehearsal,
}

#[derive(Clone, Copy, ValueEnum)]
enum ReviewFormat {
    Markdown,
    Github,
    Html,
    Sarif,
    Gitlab,
}

pub(crate) fn run(args: ReviewArgs) -> Result<u8, Failure> {
    let format = match (args.kind, args.format) {
        (ReviewKind::Analysis, _) => None,
        (_, ReviewFormat::Markdown) => Some(EvidenceFormat::Markdown),
        (_, ReviewFormat::Html) => Some(EvidenceFormat::Html),
        _ => {
            return Err(Failure::invalid(
                "diff and rehearsal reports support Markdown and HTML review",
            ));
        }
    };
    let bytes = read_input(&args.input, MAX_SAVED_REPORT_BYTES).map_err(|_| {
        Failure::operational("could not read the saved report", OPERATIONAL_FAILURE)
    })?;
    let input = std::str::from_utf8(&bytes)
        .map_err(|_| Failure::invalid("saved report must be UTF-8 JSON"))?;
    let output = args.output.as_deref();
    match (args.kind, format) {
        (ReviewKind::Analysis, _) => {
            let report = ReportV1::from_json(input)
                .map_err(|_| Failure::invalid("invalid versioned report JSON"))?;
            write_report(output, |out| match args.format {
                ReviewFormat::Markdown => dmg_report::write_markdown(&report, out),
                ReviewFormat::Github => dmg_report::write_github_annotations(&report, out),
                ReviewFormat::Html => dmg_report::write_html(&report, out),
                ReviewFormat::Sarif => dmg_report::write_sarif(&report, out),
                ReviewFormat::Gitlab => dmg_report::write_gitlab_code_quality(&report, out),
            })
        }
        (ReviewKind::Diff, Some(format)) => {
            let report = DiffReportV1::from_json(input)
                .map_err(|error| Failure::invalid(error.to_string()))?;
            write_report(output, |out| {
                dmg_report::write_diff_review(&report, format, out)
            })
        }
        (ReviewKind::Rehearsal, Some(format)) => {
            let report: RehearsalReportV1 = serde_json::from_str(input)
                .map_err(|_| Failure::invalid("invalid versioned rehearsal report JSON"))?;
            report
                .validate()
                .map_err(|error| Failure::invalid(error.to_string()))?;
            write_report(output, |out| {
                dmg_report::write_rehearsal_review(&report, format, out)
            })
        }
        (_, None) => unreachable!("all supported evidence formats are set above"),
    }
    .map_err(|error| {
        if error.kind() == io::ErrorKind::InvalidData {
            Failure::invalid("saved report contains an invalid review source location")
        } else {
            Failure::operational("could not write the review output", OPERATIONAL_FAILURE)
        }
    })?;
    Ok(0)
}
