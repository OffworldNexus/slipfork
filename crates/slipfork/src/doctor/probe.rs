//! Composition-root probes for `slipfork doctor`.
//!
//! This is the only place outside `main.rs` that reads `/proc` or spawns a host tool; it is
//! reached exclusively from the CLI entry point, which injects `PATH`, the project, and the state
//! directory. The renderer never probes anything.

use std::fs;
use std::os::unix::fs::FileTypeExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use super::Check;
use crate::world::HostEnv;

/// One styled fragment of a doctor row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Part {
    /// A filesystem path.
    Path(String),
    /// Secondary prose.
    Meta(String),
    /// A bold label.
    Bold(String),
    /// A primary URL.
    Url(String),
    /// A healthy state word.
    Ok(String),
    /// A failed state word.
    Bad(String),
}

impl Part {
    /// The plain text this fragment contributes to `--json`.
    #[must_use]
    pub fn text(&self) -> &str {
        match self {
            Self::Path(text)
            | Self::Meta(text)
            | Self::Bold(text)
            | Self::Url(text)
            | Self::Ok(text)
            | Self::Bad(text) => text,
        }
    }

    /// The same fragment under the shipped palette.
    #[must_use]
    pub fn render(&self, palette: &crate::ui::report::Palette) -> String {
        match self {
            Self::Path(text) => palette.path(text),
            Self::Meta(text) => palette.meta(text),
            Self::Bold(text) => palette.label(text),
            Self::Url(text) => palette.url(text),
            Self::Ok(text) => palette.status(text),
            Self::Bad(text) => palette.error(text),
        }
    }
}

/// The resolved host verdict for the four host requirements.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Doctor {
    /// `Storage` row.
    pub storage: Check,
    /// `Containers` row.
    pub containers: Check,
    /// `Daemon` row.
    pub daemon: Check,
    /// `Domains` row.
    pub domains: Check,
}

impl Doctor {
    /// Probes the injected host state once.
    #[must_use]
    pub fn probe(host: &HostEnv, cwd: &Path) -> Self {
        Self {
            storage: storage(cwd, &host.state_dir, host.home.as_deref()),
            containers: containers(host),
            daemon: daemon(host),
            domains: domains(host),
        }
    }
}

fn storage(project: &Path, state_dir: &Path, home: Option<&Path>) -> Check {
    let project_is_btrfs = slipfork_world::is_btrfs_path(project).unwrap_or(false);
    let state_is_btrfs = slipfork_world::is_btrfs_path(state_dir).unwrap_or(false);
    storage_row(&shorten(project, home), project_is_btrfs || state_is_btrfs)
}

/// The shipped `Storage` copy: the path, one bold verdict phrase, one consequence.
///
/// Three parts means one separator each; the verdict is a single bold phrase so `--json` never
/// reads `~/demo · on · Btrfs · …`.
fn storage_row(shown: &str, on_btrfs: bool) -> Check {
    if on_btrfs {
        return Check::new(
            "Storage",
            true,
            vec![
                Part::Path(shown.to_owned()),
                Part::Bold("on Btrfs".to_owned()),
                Part::Meta("instant copy-on-write forks ready".to_owned()),
            ],
            None,
        );
    }
    Check::new(
        "Storage",
        false,
        vec![
            Part::Path(shown.to_owned()),
            Part::Bold("not on Btrfs".to_owned()),
            Part::Meta("instant copy-on-write forks are unavailable".to_owned()),
        ],
        Some("move the project onto a Btrfs mount and run `slipfork up`".to_owned()),
    )
}

/// Whether rootless container sandboxing can start on this host.
enum Sandbox {
    /// User namespaces work.
    Enabled,
    /// AppArmor denies the user-namespace clone.
    AppArmor,
    /// The kernel or sysctl refuses user namespaces.
    Unavailable,
}

fn containers(host: &HostEnv) -> Check {
    let Some(crun) = find(&host.path, "crun") else {
        return Check::new(
            "Containers",
            false,
            vec![
                Part::Bold("crun not installed".to_owned()),
                Part::Meta("rootless container sandboxing unavailable".to_owned()),
            ],
            Some("install crun (for example `sudo apt-get install crun`) and retry".to_owned()),
        );
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
    let label = format!("crun {version}");
    let (mut ready, mut parts, mut remedy) = match sandbox(&host.path) {
        Sandbox::Enabled => (
            true,
            vec![
                Part::Bold(label),
                Part::Meta("rootless container sandboxing enabled".to_owned()),
            ],
            None,
        ),
        Sandbox::AppArmor => (
            false,
            vec![
                Part::Bold(label),
                Part::Meta("rootless container sandboxing blocked by AppArmor".to_owned()),
            ],
            Some(
                "sudo sysctl -w kernel.apparmor_restrict_unprivileged_userns=0 and retry"
                    .to_owned(),
            ),
        ),
        Sandbox::Unavailable => (
            false,
            vec![
                Part::Bold(label),
                Part::Meta("rootless container sandboxing unavailable".to_owned()),
            ],
            Some("sudo sysctl -w user.max_user_namespaces=2147483647 and retry".to_owned()),
        ),
    };
    // One row carries both tools a rootless world needs: `crun` for pods and `slirp4netns` for
    // world egress. Missing egress demotes the row but never hides the sandbox verdict.
    if find(&host.path, "slirp4netns").is_none() {
        ready = false;
        parts.push(Part::Bold("slirp4netns not installed".to_owned()));
        parts.push(Part::Meta(
            "pods cannot reach PyPI, npm, or HTTPS".to_owned(),
        ));
        let egress_remedy =
            "install slirp4netns (for example `sudo apt install slirp4netns`) and retry";
        remedy = Some(match remedy {
            Some(sandbox) => format!("{sandbox}; {egress_remedy}"),
            None => egress_remedy.to_owned(),
        });
    }
    Check::new("Containers", ready, parts, remedy)
}

fn sandbox(path: &[PathBuf]) -> Sandbox {
    let maximum = fs::read_to_string("/proc/sys/user/max_user_namespaces")
        .ok()
        .and_then(|text| text.trim().parse::<u64>().ok());
    let clone = find(path, "unshare").is_some_and(|binary| {
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
    if clone && maximum.is_none_or(|maximum| maximum > 0) {
        return Sandbox::Enabled;
    }
    if apparmor_enabled() {
        Sandbox::AppArmor
    } else {
        Sandbox::Unavailable
    }
}

fn apparmor_enabled() -> bool {
    fs::read_to_string("/sys/module/apparmor/parameters/enabled")
        .is_ok_and(|text| text.trim().eq_ignore_ascii_case("Y"))
}

fn daemon(host: &HostEnv) -> Check {
    // `daemon.sock` is a Unix socket, so `Path::is_file()` is always false; check the socket bit.
    let listening = fs::metadata(host.runtime_dir.join("daemon.sock"))
        .is_ok_and(|metadata| metadata.file_type().is_socket());
    if listening {
        return Check::new(
            "Daemon",
            true,
            vec![
                Part::Ok("running".to_owned()),
                Part::Meta("background user service active".to_owned()),
            ],
            None,
        );
    }
    Check::new(
        "Daemon",
        false,
        vec![
            Part::Bad("not running".to_owned()),
            Part::Meta("`slipfork up` starts it automatically".to_owned()),
        ],
        Some(
            "run `slipfork up` (packages enable `slipfork.service`; otherwise \
             `systemctl --user enable --now slipfork`)"
                .to_owned(),
        ),
    )
}

/// The ingress socket unit as shipped: packages use `/usr/lib`, hand installs `/etc`.
fn ingress_socket(install_root: &Path) -> Option<PathBuf> {
    ["etc/systemd/system", "usr/lib/systemd/system"]
        .iter()
        .map(|directory| install_root.join(directory).join("slipfork-ingress.socket"))
        .find(|path| path.is_file())
}

/// Whether OpenRC ships the ingress service under this install root.
fn openrc_ingress(install_root: &Path) -> bool {
    install_root.join("etc/init.d/slipfork-ingress").is_file()
}

fn domains(host: &HostEnv) -> Check {
    let active = find(&host.path, "systemctl").is_some_and(|systemctl| {
        Command::new(systemctl)
            .args(["is-active", "--quiet", "slipfork-ingress.socket"])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .is_ok_and(|status| status.success())
    }) || find(&host.path, "rc-service").is_some_and(|rc_service| {
        Command::new(rc_service)
            .args(["slipfork-ingress", "status"])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .is_ok_and(|status| status.success())
    });
    if active {
        return Check::new(
            "Domains",
            true,
            vec![
                Part::Url("http://*.localhost".to_owned()),
                Part::Meta("local domain routing active".to_owned()),
            ],
            None,
        );
    }
    let openrc = openrc_ingress(&host.install_root);
    let socket = ingress_socket(&host.install_root);
    let detail = if openrc || socket.is_some() {
        "local domain routing inactive"
    } else {
        "local domain routing not installed"
    };
    let next = if openrc {
        "start it with `sudo rc-service slipfork-ingress start`"
    } else if socket.is_some() {
        "enable it with `sudo systemctl enable --now slipfork-ingress.socket`"
    } else {
        "packages enable `slipfork-ingress.socket`; otherwise copy \
         `/usr/share/slipfork/systemd/*` into the unit directories, then \
         `sudo systemctl enable --now slipfork-ingress.socket`"
    };
    Check::new(
        "Domains",
        false,
        vec![
            Part::Url("http://*.localhost".to_owned()),
            Part::Meta(detail.to_owned()),
        ],
        Some(next.to_owned()),
    )
}

fn shorten(path: &Path, home: Option<&Path>) -> String {
    let Some(home) = home else {
        return path.display().to_string();
    };
    let Ok(rest) = path.strip_prefix(home) else {
        return path.display().to_string();
    };
    if rest.as_os_str().is_empty() {
        "~".to_owned()
    } else {
        format!("~/{}", rest.display())
    }
}

fn find(path: &[PathBuf], name: &str) -> Option<PathBuf> {
    path.iter()
        .map(|directory| directory.join(name))
        .find(|candidate| candidate.is_file())
}

#[cfg(test)]
mod tests {
    use super::storage_row;

    #[test]
    fn storage_row_is_three_parts_and_one_separator_each() {
        let ready = storage_row("~/demo", true);
        assert_eq!(
            ready.detail,
            "~/demo · on Btrfs · instant copy-on-write forks ready"
        );
        assert_eq!(ready.parts.len(), 3, "one separator per seam: {ready:?}");
        assert_eq!(ready.detail.matches(" · ").count(), 2, "{:?}", ready.detail);
        assert!(ready.ok);
        assert_eq!(ready.next, None);

        let missing = storage_row("~/demo", false);
        assert_eq!(
            missing.detail,
            "~/demo · not on Btrfs · instant copy-on-write forks are unavailable"
        );
        assert_eq!(
            missing.parts.len(),
            3,
            "one separator per seam: {missing:?}"
        );
        assert_eq!(
            missing.detail.matches(" · ").count(),
            2,
            "{:?}",
            missing.detail
        );
        assert!(!missing.ok);
        assert!(missing.next.is_some());
    }
}
