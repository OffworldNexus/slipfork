//! Subvolume teardown: drop one clone, a world's ephemeral pod clones, or a world tree child-first.

use std::fs;
use std::io;
use std::path::Path;

use crate::btrfs;
use crate::error::{Error, Result};

use super::BtrfsEngine;

/// Destroys `path` when it exists; an entry already unlinked by a racing reaper is `Ok(())`.
///
/// A fork checkout may be a Btrfs subvolume (a subvolume checkout) or a plain copied directory
/// (a fork of a plain checkout), so a non-subvolume is removed with `remove_dir_all`.
pub(super) fn destroy_if_present(path: &Path) -> Result<()> {
    if !path.exists() {
        return Ok(());
    }
    match btrfs::is_subvolume_path(path) {
        Ok(true) => match BtrfsEngine::destroy_subvolume(path) {
            Ok(()) => Ok(()),
            Err(Error::Io { source, .. }) if source.kind() == io::ErrorKind::NotFound => Ok(()),
            Err(error) => Err(error),
        },
        Ok(false) => {
            if let Err(source) = fs::remove_dir_all(path)
                && source.kind() != io::ErrorKind::NotFound
            {
                return Err(Error::io("remove", path, source));
            }
            Ok(())
        }
        Err(Error::Io { source, .. }) if source.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error),
    }
}

/// Destroys a managed `worlds/<id>` subvolume after clearing any nested pod clones inside it.
pub(super) fn destroy_world_if_present(path: &Path) -> Result<()> {
    if !path.exists() {
        return Ok(());
    }
    let mut children = Vec::new();
    let mut stack = vec![path.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let Ok(entries) = fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let Ok(file_type) = entry.file_type() else {
                continue;
            };
            if !file_type.is_dir() || file_type.is_symlink() {
                continue;
            }
            let child = entry.path();
            match btrfs::is_subvolume_path(&child) {
                Ok(true) => children.push(child.clone()),
                Ok(false) => {}
                Err(Error::Io { source, .. }) if source.kind() == io::ErrorKind::NotFound => {
                    continue;
                }
                Err(error) => return Err(error),
            }
            stack.push(child);
        }
    }
    children.sort_by_key(|child| std::cmp::Reverse(child.components().count()));
    for child in children {
        destroy_if_present(&child)?;
    }
    destroy_if_present(path)
}

/// Destroys every ephemeral pod rootfs subvolume inside `<store>/pods/<world>` and removes the dir.
pub(super) fn sweep_pods_dir(pods_dir: &Path) -> Result<()> {
    if !pods_dir.exists() {
        return Ok(());
    }
    let entries = match fs::read_dir(pods_dir) {
        Ok(entries) => entries,
        Err(source) if source.kind() == io::ErrorKind::NotFound => return Ok(()),
        Err(source) => return Err(Error::io("read", pods_dir, source)),
    };
    for entry in entries {
        let entry = match entry {
            Ok(entry) => entry,
            Err(source) if source.kind() == io::ErrorKind::NotFound => continue,
            Err(source) => return Err(Error::io("read", pods_dir, source)),
        };
        let path = entry.path();
        match btrfs::is_subvolume_path(&path) {
            Ok(true) => destroy_if_present(&path)?,
            Ok(false) if path.is_dir() => {
                if let Err(source) = fs::remove_dir_all(&path)
                    && source.kind() != io::ErrorKind::NotFound
                {
                    return Err(Error::io("remove", &path, source));
                }
            }
            Ok(false) => {
                if let Err(source) = fs::remove_file(&path)
                    && source.kind() != io::ErrorKind::NotFound
                {
                    return Err(Error::io("remove", &path, source));
                }
            }
            Err(Error::Io { source, .. }) if source.kind() == io::ErrorKind::NotFound => {}
            Err(error) => return Err(error),
        }
    }
    let _ = fs::remove_dir(pods_dir);
    if let Some(root) = pods_dir.parent() {
        let _ = fs::remove_dir(root);
    }
    Ok(())
}
