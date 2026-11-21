//! Subvolume teardown: drop one clone, or a whole managed world tree child-first.

use std::fs;
use std::path::Path;

use crate::btrfs;
use crate::error::Result;

use super::BtrfsEngine;

/// Destroys `path` when it exists.
pub(super) fn destroy_if_present(path: &Path) -> Result<()> {
    if path.exists() {
        BtrfsEngine::destroy_subvolume(path)
    } else {
        Ok(())
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
            if btrfs::is_subvolume_path(&child)? {
                children.push(child.clone());
            }
            stack.push(child);
        }
    }
    children.sort_by_key(|child| std::cmp::Reverse(child.components().count()));
    for child in children {
        BtrfsEngine::destroy_subvolume(&child)?;
    }
    BtrfsEngine::destroy_subvolume(path)
}
