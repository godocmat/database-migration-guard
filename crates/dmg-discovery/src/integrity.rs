use std::collections::{BTreeMap, BTreeSet};

use dmg_core::migration::{
    FileDigest, IntegrityBasis, IntegrityChange, IntegrityComparison, IntegrityStatus,
    MigrationDirection, MigrationFile, MigrationIdentity,
};

type Key = (MigrationIdentity, MigrationDirection);

pub(super) fn file_digests(files: &[MigrationFile]) -> BTreeMap<Key, FileDigest> {
    files
        .iter()
        .map(|file| {
            (
                (file.identity.clone(), file.direction),
                FileDigest {
                    path: file.path.clone(),
                    digest: file.digest,
                },
            )
        })
        .collect()
}

pub(super) fn compare(
    basis: IntegrityBasis,
    before: BTreeMap<Key, FileDigest>,
    after: &[MigrationFile],
) -> IntegrityComparison {
    let after = file_digests(after);
    let keys = before.keys().chain(after.keys()).collect::<BTreeSet<_>>();
    let changes = keys
        .into_iter()
        .map(|key| {
            let before = before.get(key).cloned();
            let after = after.get(key).cloned();
            let status = match (&before, &after) {
                (Some(before), Some(after)) if before.digest == after.digest => {
                    IntegrityStatus::Unchanged
                }
                (Some(_), Some(_)) => IntegrityStatus::Modified,
                (Some(_), None) => IntegrityStatus::Missing,
                (None, Some(_)) => IntegrityStatus::New,
                (None, None) => unreachable!("keys come from the before and after inventories"),
            };
            IntegrityChange {
                identity: key.0.clone(),
                direction: key.1,
                status,
                before,
                after,
            }
        })
        .collect();
    IntegrityComparison { basis, changes }
}

pub(super) fn select_changed(comparison: &IntegrityComparison) -> Vec<dmg_core::source::RepoPath> {
    comparison
        .changes
        .iter()
        .filter(|change| {
            change.direction == MigrationDirection::Forward
                && (change.status != IntegrityStatus::Unchanged
                    || change.before.as_ref().map(|file| &file.path)
                        != change.after.as_ref().map(|file| &file.path))
        })
        .filter_map(|change| change.after.as_ref().map(|file| file.path.clone()))
        .collect()
}
