//! Checkout copying: reproduce a plain (non-subvolume) project into a fork directory.
//!
//! `fork` on a checkout that is not itself a Btrfs subvolume cannot use `SNAP_CREATE_V2`, so it
//! clones the tree into `<project>/.forks/<fork>` instead. The user's original checkout is only
//! read; nothing is renamed, removed, or replaced.

use std::fs;
use std::os::unix::fs::symlink;
use std::path::Path;

use crate::error::{Error, Result};
use crate::layout;

/// Recursively copies `from` into `to`, preserving directory/file modes and real symlinks.
///
/// Directories are recreated with their mode, regular files through `fs::copy` (Btrfs answers
/// that from `copy_file_range`, so payloads stay reflink/CoW-backed), and symlinks from
/// `read_link` so they are never followed. A FIFO, socket, or device node has no meaning in a
/// checkout and fails closed, as does a nested mount point.
///
/// The top-level `.forks` and `.slipfork-store` entries are skipped: `.forks` holds Slipfork's own
/// fork tree (copying it in would recurse into the destination) and `.slipfork-store` holds the
/// Btrfs store, which must never be copied into a checkout even at the mount root.
pub fn copy_checkout(from: &Path, to: &Path) -> Result<()> {
    use std::os::unix::fs::MetadataExt;

    let metadata = fs::metadata(from).map_err(|source| Error::io("inspect", from, source))?;
    let root_device = metadata.dev();
    fs::create_dir_all(to).map_err(|source| Error::io("create", to, source))?;
    fs::set_permissions(to, metadata.permissions())
        .map_err(|source| Error::io("chmod", to, source))?;
    copy_dir(from, to, root_device, true)
}

/// Reproduces the entries of `from` inside the already-created `to`.
fn copy_dir(from: &Path, to: &Path, root_device: u64, top: bool) -> Result<()> {
    use std::os::unix::fs::MetadataExt;

    for entry in fs::read_dir(from).map_err(|source| Error::io("read", from, source))? {
        let entry = entry.map_err(|source| Error::io("read", from, source))?;
        let name = entry.file_name();
        if top && layout::is_managed_entry(&name) {
            continue;
        }
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
            copy_dir(&source_path, &target_path, root_device, false)?;
        } else if metadata.file_type().is_symlink() {
            let target = fs::read_link(&source_path)
                .map_err(|source| Error::io("read", &source_path, source))?;
            symlink(&target, &target_path)
                .map_err(|source| Error::io("symlink", &target_path, source))?;
        } else if metadata.is_file() {
            fs::copy(&source_path, &target_path)
                .map_err(|source| Error::io("copy", &source_path, source))?;
            fs::set_permissions(&target_path, metadata.permissions())
                .map_err(|source| Error::io("chmod", &target_path, source))?;
        } else {
            return Err(Error::UnsupportedEntry { path: source_path });
        }
    }
    Ok(())
}

#[cfg(test)]
#[allow(unsafe_code)]
mod tests {
    use std::ffi::CString;
    use std::fs;
    use std::os::unix::ffi::OsStrExt;
    use std::os::unix::fs::{PermissionsExt, symlink};
    use std::path::Path;

    use tempfile::{Builder, TempDir};

    use super::copy_checkout;
    use crate::error::Error;

    /// Real disk scratch space; `/tmp` is RAM-backed and must never hold a copied checkout.
    fn scratch(prefix: &str) -> TempDir {
        Builder::new()
            .prefix(prefix)
            .tempdir_in("/var/tmp")
            .expect("create a disk temp directory")
    }

    fn mode(path: &Path) -> u32 {
        fs::metadata(path)
            .expect("stat a copied entry")
            .permissions()
            .mode()
            & 0o777
    }

    #[test]
    fn copies_tree_modes_symlinks_and_skips_forks() {
        let root = scratch("slipfork-copy-checkout");
        let from = root.path().join("checkout");
        let to = root.path().join("forks/feat");
        fs::create_dir_all(from.join("src/nested")).expect("seed nested tree");
        fs::write(from.join("src/nested/main.rs"), "pub fn nested() {}\n")
            .expect("seed nested file");
        fs::set_permissions(from.join("src/nested"), fs::Permissions::from_mode(0o750))
            .expect("pin the directory mode");
        fs::write(from.join("run.sh"), "#!/bin/sh\necho demo\n").expect("seed script");
        fs::set_permissions(from.join("run.sh"), fs::Permissions::from_mode(0o755))
            .expect("pin the executable mode");
        symlink("src/nested/main.rs", from.join("link")).expect("seed a relative symlink");
        fs::create_dir_all(from.join(".forks/child")).expect("seed a stale fork tree");
        fs::write(from.join(".forks/child/leak"), "leak\n").expect("seed a stale fork payload");
        fs::create_dir_all(from.join(".slipfork-store/worlds/demo"))
            .expect("seed the Btrfs store tree");
        fs::write(from.join(".slipfork-store/worlds/demo/PG_VERSION"), "18\n")
            .expect("seed a store payload");

        copy_checkout(&from, &to).expect("copy the checkout");

        assert_eq!(
            fs::read_to_string(to.join("src/nested/main.rs")).expect("read the nested file"),
            "pub fn nested() {}\n"
        );
        assert_eq!(
            mode(&to.join("src/nested")),
            0o750,
            "directory mode preserved"
        );
        assert_eq!(mode(&to.join("run.sh")), 0o755, "executable mode preserved");
        assert!(
            fs::symlink_metadata(to.join("link"))
                .expect("probe the copied symlink")
                .file_type()
                .is_symlink(),
            "a symlink stays a link, never a followed copy"
        );
        assert_eq!(
            fs::read_link(to.join("link")).expect("read the copied symlink"),
            Path::new("src/nested/main.rs")
        );
        assert!(
            !to.join(".forks").exists(),
            "`.forks` must never be copied into the fork tree"
        );
        assert!(
            !to.join(".slipfork-store").exists(),
            "`.slipfork-store` must never be copied into the fork tree"
        );
    }

    #[test]
    fn refuses_a_fifo() {
        let root = scratch("slipfork-copy-fifo");
        let from = root.path().join("checkout");
        let to = root.path().join("forks/feat");
        fs::create_dir_all(&from).expect("seed the checkout");
        let fifo = from.join("pipe");
        let path = CString::new(fifo.as_os_str().as_bytes()).expect("encode the FIFO path");
        // SAFETY: `path` is a NUL-terminated live path and `mkfifo(3)` only reads it.
        let created = unsafe { libc::mkfifo(path.as_ptr(), 0o644) };
        assert_eq!(created, 0, "mkfifo must succeed to exercise the refusal");

        let error = copy_checkout(&from, &to).expect_err("a FIFO must fail closed");
        assert!(
            matches!(error, Error::UnsupportedEntry { .. }),
            "expected UnsupportedEntry, got {error:?}"
        );
    }
}
