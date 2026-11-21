//! Composition-root probes for `slipfork doctor`.
//!
//! This is the only place outside `main.rs` that reads `/proc` or spawns a host tool; it is
//! reached exclusively from the CLI entry point, which injects `PATH`, the project, and the state
//! directory. The renderer never probes anything.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use super::Check;
use crate::world::HostEnv;

/// The resolved host verdict for the three host requirements.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Doctor {
    /// `Btrfs` row.
    pub btrfs: Check,
    /// `Namespaces` row.
    pub namespaces: Check,
    /// `Runtime` row.
    pub runtime: Check,
}

impl Doctor {
    /// Probes the injected host state once.
    #[must_use]
    pub fn probe(host: &HostEnv, cwd: &Path) -> Self {
        Self {
            btrfs: btrfs(cwd, &host.state_dir),
            namespaces: namespaces(&host.path),
            runtime: runtime(host),
        }
    }
}

fn btrfs(project: &Path, state_dir: &Path) -> Check {
    let project_is_btrfs = slipfork_world::is_btrfs_path(project).unwrap_or(false);
    let state_is_btrfs = slipfork_world::is_btrfs_path(state_dir).unwrap_or(false);
    if project_is_btrfs || state_is_btrfs {
        let path = if project_is_btrfs { project } else { state_dir };
        return Check {
            name: "Btrfs",
            ok: true,
            detail: format!("{} is btrfs (subvolume snapshots enabled)", path.display()),
            next: None,
        };
    }
    Check {
        name: "Btrfs",
        ok: false,
        detail: format!("{} is not on a btrfs filesystem", project.display()),
        next: Some("move the project onto a btrfs mount and run `slipfork up`".to_owned()),
    }
}

fn namespaces(path: &[PathBuf]) -> Check {
    let maximum = fs::read_to_string("/proc/sys/user/max_user_namespaces")
        .ok()
        .and_then(|text| text.trim().parse::<u64>().ok());
    let unshare = find(path, "unshare").is_some_and(|binary| {
        let Some(true_binary) = find(path, "true") else {
            return false;
        };
        Command::new(binary)
            .arg("--user")
            .arg(true_binary)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .is_ok_and(|status| status.success())
    });
    match (maximum, unshare) {
        (Some(maximum), true) if maximum > 0 => Check {
            name: "Namespaces",
            ok: true,
            detail: "unprivileged user + netns available".to_owned(),
            next: None,
        },
        (Some(maximum), true) => Check {
            name: "Namespaces",
            ok: false,
            detail: format!("user.max_user_namespaces is {maximum}"),
            next: Some("sudo sysctl -w user.max_user_namespaces=2147483647".to_owned()),
        },
        (_, _) => Check {
            name: "Namespaces",
            ok: false,
            detail: "unprivileged user + netns unavailable".to_owned(),
            next: Some("sudo sysctl -w kernel.unprivileged_userns_clone=1".to_owned()),
        },
    }
}

fn runtime(host: &HostEnv) -> Check {
    let Some(crun) = find(&host.path, "crun") else {
        return Check {
            name: "Runtime",
            ok: false,
            detail: "crun is not on PATH".to_owned(),
            next: Some(
                "install crun (for example `sudo apt-get install crun`) and retry".to_owned(),
            ),
        };
    };
    let version = Command::new(crun)
        .arg("--version")
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .output()
        .ok()
        .map(|output| String::from_utf8_lossy(&output.stdout).into_owned())
        .and_then(|text| {
            let first = text.lines().next()?.trim();
            first
                .split_once("version ")
                .map(|(_, version)| version.trim().to_owned())
                .filter(|version| !version.is_empty())
        })
        .unwrap_or_else(|| "installed".to_owned());
    Check {
        name: "Runtime",
        ok: true,
        detail: format!(
            "crun {version}, :80 .localhost ingress {}",
            ingress_state(host)
        ),
        next: None,
    }
}

fn ingress_state(host: &HostEnv) -> &'static str {
    let active = find(&host.path, "systemctl").is_some_and(|systemctl| {
        Command::new(systemctl)
            .args(["is-active", "--quiet", "slipfork-ingress.socket"])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .is_ok_and(|status| status.success())
    });
    if active {
        "active"
    } else if crate::install::system_socket_path(&host.install_root).is_file() {
        "installed but not active"
    } else {
        "not installed (run `sudo slipfork daemon install --ingress`)"
    }
}

fn find(path: &[PathBuf], name: &str) -> Option<PathBuf> {
    path.iter()
        .map(|directory| directory.join(name))
        .find(|candidate| candidate.is_file())
}
