//! Rootfs-anchored executable and symlink resolution for container launches.
//!
//! Checking `rootfs.join(path).is_file()` on the host lets the host kernel follow absolute
//! symlinks (`/app/.venv/bin/python -> /usr/local/bin/python3.13`, merged-`/usr` `/bin -> /usr/bin`)
//! against the **host** `/`: a binary present inside the image is skipped when the host lacks that
//! path, and a broken link inside the image falsely passes when the host happens to carry the
//! target. Walking components against `rootfs` keeps every `/` and `..` clamped inside the image
//! while leaving `argv[0]` at the symlink path so Python `PEP 405` `pyvenv.cfg` discovery works.

use std::collections::VecDeque;
use std::ffi::OsString;
use std::fs;
use std::path::{Component, Path, PathBuf};

/// Kernel `MAXSYMLINKS` bound (`include/linux/namei.h`), preventing cyclic link loops.
const MAX_SYMLINKS: usize = 40;

#[derive(Debug, Clone)]
enum Segment {
    Root,
    Cur,
    Parent,
    Normal(OsString),
}

fn segments(path: &Path) -> Vec<Segment> {
    path.components()
        .map(|component| match component {
            Component::RootDir | Component::Prefix(_) => Segment::Root,
            Component::CurDir => Segment::Cur,
            Component::ParentDir => Segment::Parent,
            Component::Normal(name) => Segment::Normal(name.to_os_string()),
        })
        .collect()
}

fn host_path(rootfs: &Path, stack: &[OsString]) -> PathBuf {
    let mut out = rootfs.to_path_buf();
    for part in stack {
        out.push(part);
    }
    out
}

/// Returns `true` when `container_path` resolves to a regular file with `/` anchored at `rootfs`.
pub(super) fn is_file_in_rootfs(rootfs: &Path, container_path: &str) -> bool {
    let mut stack: Vec<OsString> = Vec::new();
    let mut pending: VecDeque<Segment> = segments(Path::new(container_path)).into();
    let mut hops = 0usize;

    while let Some(segment) = pending.pop_front() {
        match segment {
            Segment::Root => stack.clear(),
            Segment::Cur => {}
            Segment::Parent => {
                stack.pop();
            }
            Segment::Normal(name) => {
                let mut candidate = host_path(rootfs, &stack);
                candidate.push(&name);
                let Ok(meta) = fs::symlink_metadata(&candidate) else {
                    return false;
                };
                if meta.file_type().is_symlink() {
                    hops += 1;
                    if hops > MAX_SYMLINKS {
                        return false;
                    }
                    let Ok(target) = fs::read_link(&candidate) else {
                        return false;
                    };
                    for next in segments(&target).into_iter().rev() {
                        pending.push_front(next);
                    }
                } else if pending.is_empty() {
                    return meta.is_file();
                } else if meta.is_dir() {
                    stack.push(name);
                } else {
                    return false;
                }
            }
        }
    }

    fs::symlink_metadata(host_path(rootfs, &stack)).is_ok_and(|meta| meta.is_file())
}

/// Returns `true` when a `/project`-bound shim exists as a file, resolving container-absolute
/// symlink targets (such as `uv`'s `.venv/bin/python -> /usr/local/bin/python3.13`) inside `rootfs`.
pub(super) fn shim_exists(shim: &Path, rootfs: &Path) -> bool {
    let mut current = shim.to_path_buf();
    for _ in 0..=MAX_SYMLINKS {
        let Ok(meta) = fs::symlink_metadata(&current) else {
            return false;
        };
        if !meta.file_type().is_symlink() {
            return meta.is_file();
        }
        let Ok(target) = fs::read_link(&current) else {
            return false;
        };
        if target.is_absolute() {
            let Some(text) = target.to_str() else {
                return false;
            };
            return is_file_in_rootfs(rootfs, text);
        }
        let Some(parent) = current.parent() else {
            return false;
        };
        current = parent.join(target);
    }
    false
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::os::unix::fs::symlink;

    use tempfile::TempDir;

    use super::super::helpers::ServiceLaunch;
    use super::is_file_in_rootfs;

    fn disk_temp(prefix: &str) -> TempDir {
        tempfile::Builder::new()
            .prefix(prefix)
            .tempdir_in("/var/tmp")
            .expect("create /var/tmp directory")
    }

    fn write_exec(root: &TempDir, relative: &str) {
        let path = root.path().join(relative);
        fs::create_dir_all(path.parent().expect("fixture parent")).expect("create fixture dirs");
        fs::write(path, b"#!/bin/sh\n").expect("write fixture binary");
    }

    #[test]
    fn venv_absolute_symlink_resolves_inside_rootfs_instead_of_host() {
        let project = disk_temp("slipfork-venv-proj-");
        let rootfs = disk_temp("slipfork-venv-rootfs-");

        // Target path intentionally absent on the host so `Path::is_file()` on the host fails.
        let missing_on_host = "/usr/local/bin/slipfork-nonexistent-host-python3.13";
        assert!(
            !std::path::Path::new(missing_on_host).exists(),
            "host must not carry the synthetic interpreter path"
        );
        write_exec(
            &rootfs,
            "usr/local/bin/slipfork-nonexistent-host-python3.13",
        );
        symlink(
            "slipfork-nonexistent-host-python3.13",
            rootfs.path().join("usr/local/bin/python"),
        )
        .expect("base-image relative python symlink");

        fs::create_dir_all(rootfs.path().join("app/.venv/bin")).expect("create venv bin");
        symlink(missing_on_host, rootfs.path().join("app/.venv/bin/python"))
            .expect("venv absolute python symlink");
        symlink("python", rootfs.path().join("app/.venv/bin/python3"))
            .expect("chained venv python3 symlink");

        let path = "/app/.venv/bin:/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin";
        let launch = ServiceLaunch::resolve(
            project.path(),
            "/app",
            rootfs.path(),
            path,
            "python -m app.worker",
        );
        assert_eq!(
            launch.argv,
            ["/app/.venv/bin/python", "-m", "app.worker"],
            "must exec `/app/.venv/bin/python` so PEP 405 pyvenv.cfg activates `/app/.venv`"
        );

        let explicit = ServiceLaunch::resolve(
            project.path(),
            "/app",
            rootfs.path(),
            path,
            "/app/.venv/bin/python3 -m app.worker",
        );
        assert_eq!(
            explicit.argv,
            ["/app/.venv/bin/python3", "-m", "app.worker"]
        );

        // Reverse host-leak guard: `/bin/sh` exists on every Linux host, yet this rootfs has no
        // `/bin/sh` — a symlink inside `rootfs` to `/bin/sh` is dangling inside the container.
        symlink("/bin/sh", rootfs.path().join("app/.venv/bin/host-leak"))
            .expect("dangling container symlink to host /bin/sh");
        assert!(
            !is_file_in_rootfs(rootfs.path(), "/app/.venv/bin/host-leak"),
            "an absolute symlink must never fall through to the host rootfs"
        );

        // Cyclic symlinks terminate cleanly without overflowing or hanging.
        symlink("loop-b", rootfs.path().join("app/.venv/bin/loop-a")).expect("loop-a");
        symlink("loop-a", rootfs.path().join("app/.venv/bin/loop-b")).expect("loop-b");
        assert!(!is_file_in_rootfs(rootfs.path(), "/app/.venv/bin/loop-a"));

        // `..` above `/` clamps to `rootfs` just like the kernel VFS root.
        assert!(is_file_in_rootfs(
            rootfs.path(),
            "/../../../../usr/local/bin/python"
        ));
    }

    #[test]
    fn project_venv_symlink_to_container_interpreter_unwraps_uv_run() {
        let project = disk_temp("slipfork-uv-proj-");
        let rootfs = disk_temp("slipfork-uv-rootfs-");
        write_exec(
            &rootfs,
            "usr/local/bin/slipfork-nonexistent-host-python3.13",
        );
        fs::create_dir_all(project.path().join(".venv/bin")).expect("create project venv");
        symlink(
            "/usr/local/bin/slipfork-nonexistent-host-python3.13",
            project.path().join(".venv/bin/python"),
        )
        .expect("uv venv python symlink into container base image");

        let launch = ServiceLaunch::resolve(
            project.path(),
            "/project",
            rootfs.path(),
            "/usr/local/bin:/usr/bin:/bin",
            "@uv run python -m app.worker",
        );
        assert_eq!(
            launch.argv,
            ["/project/.venv/bin/python", "-m", "app.worker"]
        );
    }
}
