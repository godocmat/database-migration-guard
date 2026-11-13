use std::collections::{BTreeMap, BTreeSet};

use dmg_core::{
    coverage::{Coverage, CoverageGapKind},
    digest::ContentDigest,
    migration::{
        AppliedHistory, AppliedHistoryBasis, AppliedMigration, AppliedStatus, FileDigest,
        IntegrityBasis, MigrationAdapter, MigrationDirection, MigrationIdentity,
    },
    source::RepoPath,
};
use serde::Deserialize;
use thiserror::Error;

use crate::{Adapter, Discovery, DiscoveryOptions, coverage_gap, integrity, valid_path};

const SCHEMA_VERSION: u32 = 1;

#[derive(Debug, Error)]
pub enum ManifestError {
    #[error("manifest digest does not match the expected SHA-256")]
    DigestMismatch,
    #[error("manifest exceeds the configured entry or byte limit")]
    Limit,
    #[error("invalid versioned migration manifest JSON")]
    Format(#[source] serde_json::Error),
    #[error("unsupported migration manifest schema version")]
    UnsupportedVersion,
    #[error("manifest adapter does not match discovery configuration")]
    WrongAdapter,
    #[error("manifest contains an unsupported migration identity")]
    InvalidIdentity,
    #[error("manifest contains an invalid migration path or direction")]
    InvalidPath,
    #[error("manifest repeats a migration identity, direction, or path")]
    Duplicate,
    #[error("reversible SQLx integrity manifest contains an unpaired migration")]
    UnpairedMigration,
}

/// Compare discovery against the original bytes recorded in a trusted manifest.
///
/// Coverage may still be incomplete after successful manifest validation. Check the resulting plan
/// for integrity evidence and selection.
pub fn compare_integrity_manifest(
    discovery: &mut Discovery,
    manifest: &[u8],
    expected_digest: ContentDigest,
) -> Result<(), ManifestError> {
    let options = &discovery.options;
    let adapter = options.adapter.kind();
    let parsed: Manifest<IntegrityFile> = parse(manifest, expected_digest, options)?;
    if parsed.adapter != adapter {
        return Err(ManifestError::WrongAdapter);
    }
    let mut baseline = BTreeMap::new();
    let mut paths = BTreeSet::new();
    for entry in parsed.migrations {
        let identity = identity(options, entry.version)?;
        if !valid_path(&entry.path) {
            return Err(ManifestError::InvalidPath);
        }
        let Some((root, filename)) = entry.path.as_str().rsplit_once('/') else {
            return Err(ManifestError::InvalidPath);
        };
        let candidate = options
            .adapter
            .candidate(filename)
            .ok()
            .flatten()
            .filter(|candidate| {
                candidate.identity == identity && candidate.direction == entry.direction
            });
        if candidate.is_none() || !options.roots.iter().any(|path| path.as_str() == root) {
            return Err(ManifestError::InvalidPath);
        }
        if !paths.insert(entry.path.clone())
            || baseline
                .insert(
                    (identity, entry.direction),
                    FileDigest {
                        path: entry.path,
                        digest: entry.sha256,
                    },
                )
                .is_some()
        {
            return Err(ManifestError::Duplicate);
        }
    }
    if matches!(
        options.adapter,
        Adapter::Sqlx(crate::SqlxLayout::Reversible)
    ) {
        for ((identity, direction), file) in &baseline {
            let other = match direction {
                MigrationDirection::Forward => MigrationDirection::Down,
                MigrationDirection::Down => MigrationDirection::Forward,
            };
            let Some(other_file) = baseline.get(&(identity.clone(), other)) else {
                return Err(ManifestError::UnpairedMigration);
            };
            if *direction == MigrationDirection::Forward
                && !crate::sqlx::matching_paths(&file.path, &other_file.path)
            {
                return Err(ManifestError::UnpairedMigration);
            }
        }
    }
    if discovery.plan.coverage.gaps().is_empty() {
        let comparison = integrity::compare(
            IntegrityBasis::Manifest {
                digest: expected_digest,
            },
            baseline,
            &discovery.plan.inventory,
        );
        discovery.plan.selected_forwards = Some(integrity::select_changed(&comparison));
        discovery.plan.integrity = Some(comparison);
    }
    Ok(())
}

/// Classify forward migrations against explicit, complete applied-history evidence.
///
/// An empty validated manifest establishes that every discovered forward is pending. If discovery
/// is incomplete, unapplied or absent local files cannot establish pending or missing history.
pub fn reconcile_applied_manifest(
    discovery: &mut Discovery,
    manifest: &[u8],
    expected_digest: ContentDigest,
) -> Result<(), ManifestError> {
    let options = &discovery.options;
    let adapter = options.adapter.kind();
    let parsed: Manifest<AppliedRecord> = parse(manifest, expected_digest, options)?;
    if parsed.adapter != adapter {
        return Err(ManifestError::WrongAdapter);
    }
    let mut applied = BTreeMap::new();
    for record in parsed.migrations {
        let identity = identity(options, record.version)?;
        if applied
            .insert(identity, (record.sha256, record.status))
            .is_some()
        {
            return Err(ManifestError::Duplicate);
        }
    }
    if !discovery.head_complete {
        let mut gaps = discovery.plan.coverage.gaps().to_vec();
        gaps.push(coverage_gap(
            CoverageGapKind::MissingAppliedHistory,
            None,
            "complete head inventory is required to reconcile applied migration history",
        ));
        discovery.plan.coverage = Coverage::from_gaps(gaps);
        return Ok(());
    }

    let local = discovery
        .plan
        .inventory
        .iter()
        .filter(|file| file.direction == MigrationDirection::Forward)
        .map(|file| (&file.identity, file.digest))
        .collect::<BTreeMap<_, _>>();
    let identities = applied
        .keys()
        .chain(local.keys().copied())
        .collect::<BTreeSet<_>>();
    let migrations = identities
        .into_iter()
        .map(|identity| {
            let status = match (local.get(identity), applied.get(identity)) {
                (_, Some((_, ManifestAppliedStatus::Failed))) => AppliedStatus::Failed,
                (Some(current), Some((expected, ManifestAppliedStatus::Applied)))
                    if current == expected =>
                {
                    AppliedStatus::Applied
                }
                (Some(_), Some((_, ManifestAppliedStatus::Applied))) => AppliedStatus::Modified,
                (None, Some((_, ManifestAppliedStatus::Applied))) => AppliedStatus::Missing,
                (Some(_), None) => AppliedStatus::Pending,
                (None, None) => {
                    unreachable!("identities come from the local and applied inventories")
                }
            };
            AppliedMigration {
                identity: identity.clone(),
                status,
            }
        })
        .collect();
    discovery.plan.applied_history = Some(AppliedHistory {
        basis: AppliedHistoryBasis::Manifest {
            digest: expected_digest,
        },
        migrations,
    });
    Ok(())
}

fn identity(options: &DiscoveryOptions, version: i64) -> Result<MigrationIdentity, ManifestError> {
    let adapter = options.adapter.kind();
    if matches!(options.adapter, Adapter::Plain(_)) && version <= 0 {
        return Err(ManifestError::InvalidIdentity);
    }
    Ok(MigrationIdentity { adapter, version })
}

fn parse<T: for<'de> Deserialize<'de>>(
    bytes: &[u8],
    digest: ContentDigest,
    options: &DiscoveryOptions,
) -> Result<Manifest<T>, ManifestError> {
    if bytes.len() > options.limits.max_file_bytes || bytes.len() > options.limits.max_total_bytes {
        return Err(ManifestError::Limit);
    }
    if ContentDigest::of(bytes) != digest {
        return Err(ManifestError::DigestMismatch);
    }
    let manifest: Manifest<T> = serde_json::from_slice(bytes).map_err(ManifestError::Format)?;
    if manifest.schema_version != SCHEMA_VERSION {
        return Err(ManifestError::UnsupportedVersion);
    }
    if manifest.migrations.len() > options.limits.max_entries {
        return Err(ManifestError::Limit);
    }
    Ok(manifest)
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Manifest<T> {
    schema_version: u32,
    adapter: MigrationAdapter,
    migrations: Vec<T>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct IntegrityFile {
    version: i64,
    direction: MigrationDirection,
    path: RepoPath,
    sha256: ContentDigest,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct AppliedRecord {
    version: i64,
    sha256: ContentDigest,
    status: ManifestAppliedStatus,
}

#[derive(Deserialize)]
#[serde(rename_all = "snake_case")]
enum ManifestAppliedStatus {
    Applied,
    Failed,
}
