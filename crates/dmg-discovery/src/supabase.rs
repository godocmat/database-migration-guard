//! Discovery semantics for Supabase CLI 3.0.0 SQL migrations.

use dmg_core::{
    coverage::{CoverageGap, CoverageGapKind},
    migration::{ExecutionMode, MigrationDirection, MigrationIdentity},
    source::{ByteRange, RepoPath},
};
use regex::Regex;
use std::sync::LazyLock;

use crate::{Candidate, SourceLayout, coverage_gap};

const BOM: &[u8] = b"\xef\xbb\xbf";
const NO_TRANSACTION: &[u8] = b"-- pg-delta: transaction=false";
const LEGACY_INIT_CUTOFF: &str = "20211209000000";
static LEGACY_INIT: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"([0-9]{14})_init\.sql").expect("valid Supabase legacy name pattern")
});

pub(super) fn candidate(filename: &str) -> Result<Option<Candidate>, &'static str> {
    let Some((version, _)) = filename
        .strip_suffix(".sql")
        .and_then(|stem| stem.split_once('_'))
        .filter(|(version, _)| {
            !version.is_empty() && version.bytes().all(|byte| byte.is_ascii_digit())
        })
    else {
        return Ok(None);
    };
    if LEGACY_INIT
        .captures(filename)
        .and_then(|capture| capture.get(1))
        .is_some_and(|timestamp| timestamp.as_str() < LEGACY_INIT_CUTOFF)
    {
        // The CLI's legacy exception depends on the first raw directory entry,
        // including names it otherwise ignores.
        return Err("Supabase legacy init selection depends on native directory order");
    }
    let identity = MigrationIdentity::supabase_version(version)
        .map_err(|_| "Supabase migration version is outside the supported size limit")?;
    Ok(Some(Candidate {
        identity,
        direction: MigrationDirection::Forward,
    }))
}

pub(super) fn source_layouts(
    path: &RepoPath,
    bytes: &[u8],
) -> (Vec<SourceLayout>, Option<CoverageGap>) {
    let sql = bytes.strip_prefix(BOM).unwrap_or(bytes);
    let first_line = sql.split(|byte| *byte == b'\n').next().unwrap_or_default();
    let first_line = first_line.strip_suffix(b"\r").unwrap_or(first_line);
    let execution = if first_line == NO_TRANSACTION {
        ExecutionMode::PSQL_AUTOCOMMIT
    } else {
        ExecutionMode::SUPABASE_BATCH
    };
    let start = bytes.len() - sql.len();
    let layout = SourceLayout {
        direction: MigrationDirection::Forward,
        source_segments: vec![ByteRange::new(start, bytes.len()).expect("valid BOM offset")],
        execution,
    };
    let gap = std::str::from_utf8(bytes).is_err().then(|| {
        coverage_gap(
            CoverageGapKind::UnsupportedInput,
            Some(path),
            "Supabase migration must be valid UTF-8 to preserve native statement boundaries",
        )
    });
    (vec![layout], gap)
}
