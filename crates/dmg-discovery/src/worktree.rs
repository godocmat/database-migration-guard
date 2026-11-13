use std::{io, io::Read, path::Path};

#[cfg(unix)]
use cap_fs_ext::OpenOptionsSyncExt;
use cap_fs_ext::{DirExt, FollowSymlinks, OpenOptionsFollowExt};
use cap_std::{
    ambient_authority,
    fs::{Dir, OpenOptions},
};
use dmg_core::{coverage::CoverageGapKind, source::RepoPath};

use crate::{
    DiscoveryOptions, Inventory, budget_for_file, coverage_gap, record_candidate, sorted_entries,
};

pub(super) fn collect(repository: &Path, options: &DiscoveryOptions, inventory: &mut Inventory) {
    let Ok(repository) = Dir::open_ambient_dir(repository, ambient_authority()) else {
        inventory.gaps.push(coverage_gap(
            CoverageGapKind::UnreadableInput,
            None,
            "could not open the repository directory",
        ));
        return;
    };

    let mut remaining_entries = options.limits.max_entries;
    for root in &options.roots {
        let directory = match open_root(&repository, root) {
            Ok(directory) => directory,
            Err(error) => {
                inventory.gaps.push(coverage_gap(
                    if error.kind() == io::ErrorKind::NotFound {
                        CoverageGapKind::UnreadableInput
                    } else {
                        CoverageGapKind::InvalidPath
                    },
                    Some(root),
                    "could not open the migration root as an ordinary directory",
                ));
                continue;
            }
        };
        let Ok(entries) = directory.entries() else {
            inventory.gaps.push(coverage_gap(
                CoverageGapKind::UnreadableInput,
                Some(root),
                "could not enumerate the migration root",
            ));
            continue;
        };

        let mut unreadable = false;
        let names = entries.filter_map(|entry| match entry {
            Ok(entry) => Some(entry.file_name()),
            Err(_) => {
                unreadable = true;
                None
            }
        });
        let Some(names) = sorted_entries(names, remaining_entries) else {
            inventory.gaps.push(coverage_gap(
                CoverageGapKind::InputLimitExceeded,
                Some(root),
                "migration directory entry limit exceeded",
            ));
            break;
        };
        if unreadable {
            inventory.gaps.push(coverage_gap(
                CoverageGapKind::UnreadableInput,
                Some(root),
                "could not enumerate all migration directory entries",
            ));
        }
        remaining_entries -= names.len();
        for name in names {
            if let Some(filename) = name.to_str() {
                collect_file(inventory, options, &directory, root, filename);
            } else {
                inventory.gaps.push(coverage_gap(
                    CoverageGapKind::InvalidPath,
                    Some(root),
                    "migration directory contains a non-UTF-8 filename",
                ));
            }
        }
    }
}

fn open_root(repository: &Dir, root: &RepoPath) -> io::Result<Dir> {
    root.as_str()
        .split('/')
        .try_fold(repository.try_clone()?, |directory, part| {
            directory.open_dir_nofollow(part)
        })
}

fn collect_file(
    inventory: &mut Inventory,
    options: &DiscoveryOptions,
    directory: &Dir,
    root: &RepoPath,
    filename: &str,
) {
    let metadata = directory.symlink_metadata(filename);
    if metadata.as_ref().is_ok_and(cap_std::fs::Metadata::is_dir) {
        return;
    }
    let Ok(path) = RepoPath::new(format!("{root}/{filename}")) else {
        inventory.gaps.push(coverage_gap(
            CoverageGapKind::InvalidPath,
            Some(root),
            "migration directory contains a nonportable filename",
        ));
        return;
    };
    let Some(candidate) = record_candidate(inventory, options, &path, filename) else {
        return;
    };
    let metadata = match metadata {
        Ok(metadata) if metadata.is_file() => metadata,
        Ok(_) => {
            inventory.gaps.push(coverage_gap(
                CoverageGapKind::InvalidPath,
                Some(&path),
                "migration input must be an ordinary file",
            ));
            return;
        }
        Err(_) => {
            inventory.gaps.push(coverage_gap(
                CoverageGapKind::UnreadableInput,
                Some(&path),
                "could not inspect migration input",
            ));
            return;
        }
    };
    let budget = budget_for_file(inventory, options);
    if metadata.len() > budget as u64 {
        inventory.gaps.push(coverage_gap(
            CoverageGapKind::InputLimitExceeded,
            Some(&path),
            "migration input exceeds the available byte budget",
        ));
        return;
    }
    match read_file(directory, filename, budget) {
        Ok(bytes) if bytes.len() <= budget => inventory.add(candidate, path, bytes),
        Ok(_) => inventory.gaps.push(coverage_gap(
            CoverageGapKind::InputLimitExceeded,
            Some(&path),
            "migration input exceeds the available byte budget",
        )),
        Err(_) => inventory.gaps.push(coverage_gap(
            CoverageGapKind::UnreadableInput,
            Some(&path),
            "could not read an ordinary migration file",
        )),
    }
}

fn read_file(directory: &Dir, name: &str, budget: usize) -> io::Result<Vec<u8>> {
    let mut options = OpenOptions::new();
    options.read(true).follow(FollowSymlinks::No);
    #[cfg(unix)]
    options.nonblock(true);
    let file = directory.open_with(name, &options)?;
    if !file.metadata()?.is_file() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "expected a regular file",
        ));
    }
    let mut bytes = Vec::new();
    file.take(budget as u64 + 1).read_to_end(&mut bytes)?;
    Ok(bytes)
}
