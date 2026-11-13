use dmg_core::{
    coverage::{CoverageGap, CoverageGapKind},
    migration::{
        ExecutionMode, MigrationDirection, MigrationIdentity, StatementDelivery, TransactionWrapper,
    },
    source::{ByteRange, RepoPath},
};

use crate::{Candidate, DiscoveryOptions, FlywaySettings, Inventory, SourceLayout, coverage_gap};

const SCRIPT_EXTENSIONS: [&str; 6] = ["bat", "bash", "cmd", "ps1", "py", "sh"];

pub(super) fn candidate(filename: &str) -> Result<Option<Candidate>, &'static str> {
    let Some((stem, extension)) = filename.rsplit_once('.') else {
        // Flyway can recognize extensionless executable migrations on Unix.
        return if runner_input_name(filename) {
            Err("Flyway script migrations and callbacks are outside the supported SQL profile")
        } else {
            Ok(None)
        };
    };
    if runner_input_name(stem)
        && SCRIPT_EXTENSIONS
            .iter()
            .any(|ext| extension.eq_ignore_ascii_case(ext))
    {
        return Err("Flyway script migrations and callbacks are outside the supported SQL profile");
    }
    if extension.eq_ignore_ascii_case("conf")
        && stem
            .rsplit_once('.')
            .is_some_and(|(_, suffix)| suffix.eq_ignore_ascii_case("sql"))
    {
        return Err("Flyway SQL configuration sidecars require revision-bound execution evidence");
    }
    if !extension.eq_ignore_ascii_case("sql") {
        // A default-style migration with a nondefault suffix can execute under custom naming.
        return if runner_input_name(stem) {
            Err("Flyway migration or callback suffix is outside the supported SQL profile")
        } else {
            Ok(None)
        };
    }

    let Some(prefix) = stem.as_bytes().first() else {
        return Err("Flyway SQL input does not match a supported migration filename");
    };
    let (identity, direction) = match prefix {
        b'V' | b'U' => {
            let version = stem[1..]
                .split_once("__")
                .map_or(&stem[1..], |(value, _)| value);
            let direction = if *prefix == b'V' {
                MigrationDirection::Forward
            } else {
                MigrationDirection::Down
            };
            (MigrationIdentity::flyway_version(version), direction)
        }
        b'R' => {
            let description = stem
                .strip_prefix("R__")
                .ok_or("Flyway repeatable SQL filename must use the R__ prefix")?;
            (
                MigrationIdentity::flyway_repeatable(description),
                MigrationDirection::Forward,
            )
        }
        _ => {
            return Err(
                "Flyway SQL callbacks, baselines and unknown naming are outside the supported profile",
            );
        }
    };
    let identity =
        identity.map_err(|_| "Flyway SQL filename has an unsupported migration identity")?;
    Ok(Some(Candidate {
        identity,
        direction,
    }))
}

fn runner_input_name(stem: &str) -> bool {
    // All pinned callback event IDs use before/after prefixes or createSchema.
    stem.contains("__")
        || stem.starts_with("before")
        || stem.starts_with("after")
        || stem.split("__").next() == Some("createSchema")
        || matches!(
            stem.as_bytes(),
            [b'V' | b'U' | b'B', b'0'..=b'9' | b'+' | b'-', ..]
        )
}

pub(super) fn source_layout(
    settings: FlywaySettings,
    candidate: &Candidate,
    path: &RepoPath,
    bytes: &[u8],
) -> (Vec<SourceLayout>, Option<CoverageGap>) {
    let bom_len = if bytes.starts_with(b"\xef\xbb\xbf") {
        3
    } else {
        0
    };
    let gap = std::str::from_utf8(bytes)
        .is_err()
        .then(|| {
            coverage_gap(
                CoverageGapKind::UnsupportedInput,
                Some(path),
                "Flyway SQL input must use UTF-8",
            )
        })
        .or_else(|| {
            (candidate.direction == MigrationDirection::Down
                && bytes.windows(2).any(|pair| pair == b"${"))
            .then(|| {
                coverage_gap(
                    CoverageGapKind::UnsupportedInput,
                    Some(path),
                    "Flyway SQL placeholder substitution is outside the supported profile",
                )
            })
        });
    (
        vec![SourceLayout {
            direction: candidate.direction,
            source_segments: vec![
                ByteRange::new(bom_len, bytes.len()).expect("BOM is part of the file"),
            ],
            execution: ExecutionMode {
                transaction_wrapper: TransactionWrapper::Flyway(settings),
                statement_delivery: StatementDelivery::PerStatement,
            },
        }],
        gap,
    )
}

pub(super) fn check_child_root(
    inventory: &mut Inventory,
    options: &DiscoveryOptions,
    path: &RepoPath,
) {
    if options.roots.binary_search(path).is_err() {
        inventory.gaps.push(coverage_gap(
            CoverageGapKind::UnsupportedInput,
            Some(path),
            "Flyway searches nested locations recursively. Declare this directory as a migration root",
        ));
    }
}
