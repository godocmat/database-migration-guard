use dmg_core::{
    coverage::{CoverageGap, CoverageGapKind, DiagnosticScope},
    migration::{
        ExecutionMode, MigrationAdapter, MigrationDirection, MigrationFile, MigrationIdentity,
    },
};

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
            identity: MigrationIdentity {
                adapter: MigrationAdapter::Sqlx,
                version,
            },
            direction,
            execution: ExecutionMode::SQLX_TRANSACTION,
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

pub(super) fn validate_pairs(files: &[MigrationFile], gaps: &mut Vec<CoverageGap>) {
    for pair in files.chunk_by(|left, right| left.identity == right.identity) {
        let first = &pair[0];
        if first.identity.adapter != MigrationAdapter::Sqlx || !is_reversible(first) {
            continue;
        }
        if pair.len() == 2
            && pair[0].direction == MigrationDirection::Forward
            && pair[1].direction == MigrationDirection::Down
            && pair_name(&pair[0]) == pair_name(&pair[1])
        {
            continue;
        }
        gaps.push(CoverageGap {
            scope: DiagnosticScope::Migration {
                identity: first.identity.clone(),
            },
            kind: CoverageGapKind::UnpairedMigration,
            message:
                "reversible SQLx migrations require matching Up and Down names within one root"
                    .into(),
            related_paths: pair.iter().map(|file| file.path.clone()).collect(),
        });
    }
}

fn is_reversible(file: &MigrationFile) -> bool {
    let (_, name) = file
        .path
        .as_str()
        .rsplit_once('/')
        .expect("migration resides below a root");
    name.split_once('_')
        .is_some_and(|(_, suffix)| suffix.ends_with(".up.sql") || suffix.ends_with(".down.sql"))
}

fn pair_name(file: &MigrationFile) -> (&str, &str) {
    let (root, filename) = file
        .path
        .as_str()
        .rsplit_once('/')
        .expect("migration resides below a root");
    let (_, name) = filename
        .split_once('_')
        .expect("SQLx filename has a version prefix");
    let stem = name
        .strip_suffix(".up.sql")
        .or_else(|| name.strip_suffix(".down.sql"))
        .unwrap_or(name);
    (root, stem)
}
