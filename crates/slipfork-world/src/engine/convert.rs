//! In-place checkout conversion: rebuild a plain directory as a Btrfs subvolume.

use std::ffi::{OsStr, OsString};
use std::fs;
use std::os::unix::fs::symlink;
use std::path::Path;

use crate::btrfs;
use crate::error::{Error, Result};
use crate::layout;

use super::BtrfsEngine;

/// In-place conversion: copy the checkout into a sibling subvolume, then rename it in.
///
/// Btrfs answers `EXDEV` to `rename(2)` across a subvolume root for every inode, not only
/// directories, so the payload is reproduced instead: directories with their mode, regular
/// files through `copy_file_range` (`fs::copy` is CoW/remap-backed on Btrfs), and symlinks as
/// links. The original is untouched until the whole copy has landed.
pub(super) fn convert_in_place(project: &Path) -> Result<()> {
    use std::os::unix::fs::MetadataExt;

    let (parent, name) = layout::split_dir(project)?;
    let parent_dir = BtrfsEngine::open_dir(&parent)?;
    let temp_name = free_sibling_name(&parent, &name)?;
    let temp_path = parent.join(&temp_name);
    btrfs::create_subvolume(&parent_dir, &temp_path, &temp_name)?;
    let root_device = fs::metadata(project)
        .map_err(|source| Error::io("inspect", project, source))?
        .dev();
    if let Err(failure) = copy_tree(project, &temp_path, root_device) {
        let _ = btrfs::destroy(&parent_dir, &temp_path, &temp_name);
        return Err(failure);
    }
    if let Err(failure) = fs::remove_dir_all(project) {
        return Err(Error::io("remove", project, failure));
    }
    fs::rename(&temp_path, project).map_err(|_| Error::LeftoverSubvolume {
        path: temp_path.clone(),
        target: project.to_path_buf(),
    })
}

/// Finds an unused `.slipfork-adopt*` sibling name next to `name`.
fn free_sibling_name(parent: &Path, name: &OsStr) -> Result<OsString> {
    let stem = name.to_string_lossy();
    for attempt in 0..64 {
        let candidate = if attempt == 0 {
            format!("{stem}.slipfork-adopt")
        } else {
            format!("{stem}.slipfork-adopt.{attempt}")
        };
        let candidate = OsString::from(candidate);
        if !parent.join(&candidate).exists() {
            return Ok(candidate);
        }
    }
    Err(Error::AdoptionCollision {
        path: parent.join(name),
    })
}

/// Reproduces `from`'s tree inside `to`, refusing to cross a nested mount point.
///
/// Directories are recreated with their mode, regular files are copied with `fs::copy` (Btrfs
/// serves that from `copy_file_range`, so payloads stay CoW/remap-backed rather than being
/// rewritten by hand), and symlinks are recreated from `read_link` so they are never followed.
/// FIFOs, sockets, and device nodes have no meaning in a checkout and fail closed.
fn copy_tree(from: &Path, to: &Path, root_device: u64) -> Result<()> {
    use std::os::unix::fs::MetadataExt;

    for entry in fs::read_dir(from).map_err(|source| Error::io("read", from, source))? {
        let entry = entry.map_err(|source| Error::io("read", from, source))?;
        let name = entry.file_name();
        let source_path = from.join(&name);
        let target_path = to.join(&name);
        let metadata = fs::symlink_metadata(&source_path)
            .map_err(|source| Error::io("inspect", &source_path, source))?;
        if metadata.is_dir() {
            if metadata.dev() != root_device {
                return Err(Error::NestedMount { path: source_path });
            }
            fs::create_dir(&target_path)
                .map_err(|source| Error::io("create", &target_path, source))?;
            fs::set_permissions(&target_path, metadata.permissions())
                .map_err(|source| Error::io("chmod", &target_path, source))?;
            copy_tree(&source_path, &target_path, root_device)?;
        } else if metadata.is_file() {
            fs::copy(&source_path, &target_path)
                .map_err(|source| Error::io("copy", &source_path, source))?;
            fs::set_permissions(&target_path, metadata.permissions())
                .map_err(|source| Error::io("chmod", &target_path, source))?;
        } else if metadata.file_type().is_symlink() {
            let target = fs::read_link(&source_path)
                .map_err(|source| Error::io("read", &source_path, source))?;
            symlink(&target, &target_path)
                .map_err(|source| Error::io("symlink", &target_path, source))?;
        } else {
            return Err(Error::UnsupportedEntry { path: source_path });
        }
    }
    Ok(())
}
