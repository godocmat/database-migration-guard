use dmg_core::{
    coverage::{CoverageGap, CoverageGapKind, DiagnosticScope},
    migration::{
        ExecutionMode, MigrationAdapter, MigrationDirection, MigrationFile, MigrationIdentity,
    },
};

use dmg_core::source::RepoPath;

use crate::Candidate;

const NO_TRANSACTION: &[u8] = b"-- no-transaction";

/// SQLx 0.8.6 migration source layout.
#[derive(Debug, Clone, Copy)]
pub enum SqlxLayout {
    Simple,
    Reversible,
}

impl SqlxLayout {
    pub(super) fn candidate(self, filename: &str) -> Result<Option<Candidate>, &'static str> {
        let Some((prefix, rest)) = filename.split_once('_') else {
            return Ok(None);
        };
        let Some(stem) = rest.strip_suffix(".sql") else {
            return Ok(None);
        };
        let version = prefix
            .parse::<i64>()
            .map_err(|_| "SQLx filename has no signed 64-bit integer migration version")?;
        let (reversible, direction) = if stem.ends_with(".up") {
            (true, MigrationDirection::Forward)
        } else if stem.ends_with(".down") {
            (true, MigrationDirection::Down)
        } else {
            (false, MigrationDirection::Forward)
        };
        if reversible != matches!(self, Self::Reversible) {
            return Err("SQLx migration filename disagrees with the configured layout");
        }
        Ok(Some(Candidate {
            identity: MigrationIdentity::numeric(MigrationAdapter::Sqlx, version),
            direction,
        }))
    }
}

pub(super) fn execution(bytes: &[u8]) -> ExecutionMode {
    if bytes.starts_with(NO_TRANSACTION) {
        ExecutionMode::SQLX_NO_TRANSACTION
    } else {
        ExecutionMode::SQLX_TRANSACTION
    }
}

pub(super) fn validate_pairs(
    layout: &crate::Adapter,
    files: &[MigrationFile],
    gaps: &mut Vec<CoverageGap>,
) {
    if !matches!(layout, crate::Adapter::Sqlx(SqlxLayout::Reversible)) {
        return;
    }
    for pair in files.chunk_by(|left, right| left.identity == right.identity) {
        if let [up, down] = pair
            && up.direction == MigrationDirection::Forward
            && down.direction == MigrationDirection::Down
            && matching_paths(&up.path, &down.path)
        {
            continue;
        }
        gaps.push(CoverageGap {
            scope: DiagnosticScope::Migration {
                identity: pair[0].identity.clone(),
            },
            kind: CoverageGapKind::UnpairedMigration,
            message:
                "reversible SQLx migrations require matching Up and Down names within one root"
                    .into(),
            related_paths: pair.iter().map(|file| file.path.clone()).collect(),
        });
    }
}

pub(super) fn matching_paths(up: &RepoPath, down: &RepoPath) -> bool {
    pair_name(up, ".up.sql").is_some_and(|name| Some(name) == pair_name(down, ".down.sql"))
}

fn pair_name<'a>(path: &'a RepoPath, suffix: &str) -> Option<(&'a str, &'a str)> {
    let (root, filename) = path.as_str().rsplit_once('/')?;
    let (_, name) = filename.split_once('_')?;
    Some((root, name.strip_suffix(suffix)?))
}
