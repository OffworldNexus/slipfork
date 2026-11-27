//! The one owner of `slipfork-world-init` resolution for a world boot.
//!
//! `SLIPFORK_WORLD_INIT` (exported by `tools/do_vm.py test` and `make live-test` after they build
//! the helper from this checkout) wins over the sibling beside the `slipfork` under test and over
//! `PATH`. A copy outside the binary's own output directory must not predate the binary under test,
//! so a live run never executes an installed `/usr/bin` helper the checkout did not build.

use std::fs;
use std::path::{Path, PathBuf};

/// The single remedy every refusal ends with; no message appends a second one.
pub(super) const INIT_REMEDY: &str =
    "next: cargo build -p slipfork-world --bin slipfork-world-init";

pub(super) fn resolve(
    exe: &Path,
    path: &[PathBuf],
    explicit: Option<&str>,
) -> Result<PathBuf, String> {
    let resolved = match explicit.map(str::trim).filter(|value| !value.is_empty()) {
        Some(raw) => {
            let named = PathBuf::from(raw);
            if !named.is_file() {
                return Err(format!(
                    "`SLIPFORK_WORLD_INIT` names `{}`, which is not a file beside the `slipfork` \
                     under test `{}`\n{INIT_REMEDY}",
                    named.display(),
                    exe.display()
                ));
            }
            named
        }
        None => {
            let sibling = exe.parent().map(|dir| dir.join("slipfork-world-init"));
            match sibling.filter(|candidate| candidate.is_file()) {
                Some(sibling) => sibling,
                None => path
                    .iter()
                    .map(|dir| dir.join("slipfork-world-init"))
                    .find(|candidate| candidate.is_file())
                    .ok_or_else(|| {
                        format!(
                            "`slipfork-world-init` is neither beside `{}` nor on PATH\n{INIT_REMEDY}",
                            exe.display()
                        )
                    })?,
            }
        }
    };
    refuse_prebuilt(exe, &resolved)?;
    Ok(resolved)
}

/// Refuses a helper older than the binary under test unless it shares that binary's output
/// directory — the build graph links both artifacts there and their link order is not stable.
fn refuse_prebuilt(exe: &Path, init: &Path) -> Result<(), String> {
    if same_output_directory(exe, init) {
        return Ok(());
    }
    let (Some(exe_time), Some(init_time)) = (modified(exe), modified(init)) else {
        return Err(format!(
            "cannot prove `{}` was built from this checkout: its timestamp or `{}`'s is \
             unreadable\n{INIT_REMEDY}",
            init.display(),
            exe.display()
        ));
    };
    if init_time >= exe_time {
        return Ok(());
    }
    Err(format!(
        "`{}` is older than the `slipfork` under test `{}`; a live run would execute an installed \
         helper the checkout did not build\n{INIT_REMEDY}",
        init.display(),
        exe.display()
    ))
}

/// Whether both files resolve to the same directory, tolerating one relative and one absolute.
fn same_output_directory(left: &Path, right: &Path) -> bool {
    match (canonical_parent(left), canonical_parent(right)) {
        (Some(left), Some(right)) => left == right,
        _ => false,
    }
}

fn canonical_parent(path: &Path) -> Option<PathBuf> {
    let canonical = fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
    canonical.parent().map(Path::to_path_buf)
}

fn modified(path: &Path) -> Option<std::time::SystemTime> {
    fs::metadata(path)
        .and_then(|metadata| metadata.modified())
        .ok()
}

#[cfg(test)]
mod tests {
    use std::time::{Duration, UNIX_EPOCH};

    use super::*;

    fn temp(prefix: &str) -> tempfile::TempDir {
        tempfile::Builder::new()
            .prefix(prefix)
            .tempdir_in("/var/tmp")
            .expect("create a disk temp directory")
    }

    /// A real executable-shaped file with a pinned mtime, so `metadata` drives the decision.
    fn artifact(directory: &Path, name: &str, seconds: u64) -> PathBuf {
        let path = directory.join(name);
        fs::write(&path, b"#!/bin/sh\nexit 0\n").expect("write the scratch binary");
        fs::OpenOptions::new()
            .write(true)
            .open(&path)
            .expect("open the scratch binary")
            .set_times(fs::FileTimes::new().set_modified(UNIX_EPOCH + Duration::from_secs(seconds)))
            .expect("pin the scratch mtime");
        path
    }

    /// The refusal must carry exactly one `next:` line, and it must name the rebuild command.
    fn only_remedy(error: &str) -> &str {
        let mut remedies = error.lines().filter(|line| line.starts_with("next: "));
        let next = remedies
            .next()
            .unwrap_or_else(|| panic!("no remedy line: {error}"));
        assert!(
            remedies.next().is_none(),
            "more than one remedy line: {error}"
        );
        next
    }

    #[test]
    fn a_world_init_beside_the_binary_wins_without_reading_path() {
        let root = temp("slipfork-init-sibling");
        let build = root.path().join("debug");
        fs::create_dir_all(&build).expect("create the build directory");
        let exe = artifact(&build, "slipfork", 200);
        let sibling = artifact(&build, "slipfork-world-init", 100);
        let stale_path = root.path().join("usr-bin");
        fs::create_dir_all(&stale_path).expect("create the PATH directory");
        artifact(&stale_path, "slipfork-world-init", 50);

        let resolved = resolve(&exe, &[stale_path], None).expect("the sibling wins");
        assert_eq!(resolved, sibling);
    }

    #[test]
    fn an_explicit_world_init_wins_over_the_sibling() {
        let root = temp("slipfork-init-explicit");
        let build = root.path().join("debug");
        fs::create_dir_all(&build).expect("create the build directory");
        let exe = artifact(&build, "slipfork", 200);
        artifact(&build, "slipfork-world-init", 300);
        let explicit_dir = root.path().join("harness");
        fs::create_dir_all(&explicit_dir).expect("create the harness directory");
        let explicit = artifact(&explicit_dir, "slipfork-world-init", 400);

        let resolved = resolve(&exe, &[], Some(explicit.to_str().expect("utf-8")))
            .expect("the explicit helper wins");
        assert_eq!(resolved, explicit);
    }

    #[test]
    fn an_explicit_world_init_older_than_the_binary_fails_closed() {
        let root = temp("slipfork-init-explicit-stale");
        let build = root.path().join("debug");
        fs::create_dir_all(&build).expect("create the build directory");
        let exe = artifact(&build, "slipfork", 200);
        artifact(&build, "slipfork-world-init", 300);
        let stale_dir = root.path().join("usr-bin");
        fs::create_dir_all(&stale_dir).expect("create the PATH directory");
        let stale = artifact(&stale_dir, "slipfork-world-init", 100);

        let error = resolve(&exe, &[stale_dir], Some(stale.to_str().expect("utf-8")))
            .expect_err("a stale explicit helper must fail closed, never fall back");
        assert!(error.contains(&format!("`{}`", stale.display())), "{error}");
        assert!(error.contains(&format!("`{}`", exe.display())), "{error}");
        let next = only_remedy(&error);
        assert_eq!(next, INIT_REMEDY, "{error}");
    }

    #[test]
    fn an_explicit_world_init_in_the_output_directory_is_accepted_when_older() {
        let root = temp("slipfork-init-explicit-sibling");
        let build = root.path().join("debug");
        fs::create_dir_all(&build).expect("create the build directory");
        let exe = artifact(&build, "slipfork", 300);
        // `make live-test` builds the helper first and Cargo may relink `slipfork` after it.
        let init = artifact(&build, "slipfork-world-init", 200);

        let resolved = resolve(&exe, &[], Some(init.to_str().expect("utf-8")))
            .expect("the build graph owns both artifacts in one directory");
        assert_eq!(resolved, init);
    }

    #[test]
    fn a_path_world_init_older_than_the_binary_fails_closed() {
        let root = temp("slipfork-init-stale");
        let build = root.path().join("debug");
        fs::create_dir_all(&build).expect("create the build directory");
        let exe = artifact(&build, "slipfork", 200);
        let stale_dir = root.path().join("usr-bin");
        fs::create_dir_all(&stale_dir).expect("create the PATH directory");
        let stale = artifact(&stale_dir, "slipfork-world-init", 100);

        let error =
            resolve(&exe, &[stale_dir], None).expect_err("a prebuilt helper must be refused");
        assert!(error.contains(&format!("`{}`", stale.display())), "{error}");
        assert!(error.contains(&format!("`{}`", exe.display())), "{error}");
        assert!(
            error.contains("older than the `slipfork` under test"),
            "{error}"
        );
        let next = only_remedy(&error);
        assert_eq!(next, INIT_REMEDY, "{error}");
    }

    #[test]
    fn a_path_world_init_not_older_than_the_binary_is_accepted() {
        let root = temp("slipfork-init-fresh");
        let build = root.path().join("debug");
        fs::create_dir_all(&build).expect("create the build directory");
        let exe = artifact(&build, "slipfork", 200);
        let installed_dir = root.path().join("usr-local-bin");
        fs::create_dir_all(&installed_dir).expect("create the install directory");
        let installed = artifact(&installed_dir, "slipfork-world-init", 300);

        let resolved = resolve(&exe, &[installed_dir], None).expect("a fresh copy is accepted");
        assert_eq!(resolved, installed);
    }

    #[test]
    fn a_missing_explicit_world_init_fails_closed_instead_of_falling_back() {
        let root = temp("slipfork-init-missing");
        let build = root.path().join("debug");
        fs::create_dir_all(&build).expect("create the build directory");
        let exe = artifact(&build, "slipfork", 200);
        artifact(&build, "slipfork-world-init", 300);
        let missing = root.path().join("missing").join("slipfork-world-init");

        let error = resolve(&exe, &[], Some(missing.to_str().expect("utf-8")))
            .expect_err("an explicit path that does not exist must never fall back");
        assert!(error.contains("SLIPFORK_WORLD_INIT"), "{error}");
        assert!(
            error.contains(&format!("`{}`", missing.display())),
            "{error}"
        );
        assert!(
            error.contains(&format!("`slipfork` under test `{}`", exe.display())),
            "{error}"
        );
        let next = only_remedy(&error);
        assert_eq!(next, INIT_REMEDY, "{error}");
    }
}
