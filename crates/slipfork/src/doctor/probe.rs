//! The concrete host probes `slipfork doctor` runs (Btrfs stays the injected engine probe).

use std::path::PathBuf;
use std::process::Command;

use serde_json::{Value, json};
use slipfork_engine::userns_remediation_hint;

pub(crate) const PROBE_PATH: &str = "/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin";

/// Outcome of one namespace probe.
///
/// Two variants only: [`NetnsCheck::Skipped`] exists exclusively on [`DoctorReport::netns`] and is
/// synthesized by the composition root, so a probe can never report `Skipped`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HostCheckOutcome {
    /// The probe command exited successfully.
    Ok,
    /// The probe command could not run or exited non-zero; `detail` is its stderr / spawn message
    /// and `hint` is the optional remediation appended to [`DoctorReport::fixes`].
    Failed {
        /// Subprocess spawn error or captured stderr message.
        detail: String,
        /// Optional remediation hint (`ADR-0010` §4).
        hint: Option<String>,
    },
}

/// `netns` evidence on [`DoctorReport`]: executed when unprivileged user namespaces work, skipped
/// otherwise (synthesized by the composition root when user namespaces already failed).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NetnsCheck {
    /// The `netns`/`ipcns`/`pidns` probe ran.
    Executed(HostCheckOutcome),
    /// `unshare --user` failed, so the `netns` probe would only restate the same denial.
    Skipped,
}

/// Advisory `:80` systemd socket-activation state (`ADR-0011` §3).
///
/// Never affects [`DoctorReport::ok`] or [`DoctorReport::fixes`]: `doctor` must work on an
/// unprivileged host that has no `systemd` units installed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum IngressCheck {
    /// `slipforkd.socket` is active and binds port `80`; `listen` names its `Listen=` entries.
    ActivePort80 {
        /// `systemctl show -p Listen` entries, in systemd's order.
        listen: Vec<String>,
    },
    /// The unit is active but does not bind port `80` (an older or hand-written unit), or its
    /// `Listen=` entries could not be read.
    ActiveOtherListeners {
        /// `systemctl show -p Listen` entries; `None` when the query failed or returned nothing.
        listen: Option<Vec<String>>,
    },
    /// The unit exists but is not active.
    Inactive,
    /// No `slipforkd.socket` unit is installed on this host.
    NotInstalled,
}

impl IngressCheck {
    /// The one remedy every non-`:80` state shares, or `None` when the ingress already is `:80`.
    ///
    /// [`Self::label`] and [`Self::to_json`] both render this single string, so the human line and
    /// the `--json` `hint` can never drift apart.
    pub(crate) fn hint(&self) -> Option<&'static str> {
        match self {
            Self::ActivePort80 { .. } => None,
            Self::ActiveOtherListeners { listen: Some(_) } => Some(
                "active without :80 (origins carry the answered port; run `sudo slipfork daemon install` to rebind :80)",
            ),
            Self::ActiveOtherListeners { listen: None } => Some(
                "active but `systemctl show -p Listen slipforkd.socket` failed; cannot confirm :80 (run `sudo systemctl show -p Listen slipforkd.socket`)",
            ),
            Self::Inactive => {
                Some("installed but inactive (run `sudo systemctl start slipforkd.socket`)")
            }
            Self::NotInstalled => {
                Some("not installed (ephemeral port; run `sudo slipfork daemon install`)")
            }
        }
    }

    /// The single human line (`docs/design/dx-0.1.0-ux.md` §2).
    pub fn label(&self) -> String {
        self.hint().map_or_else(
            || ":80 active (port-less origins)".to_owned(),
            str::to_owned,
        )
    }

    /// The single `--json` projection (`docs/design/dx-0.1.0-ux.md` §10).
    pub fn to_json(&self) -> Value {
        match self {
            Self::ActivePort80 { listen } => json!({
                "status": "active",
                "port": 80,
                "listen": listen,
                "hint": Value::Null,
            }),
            Self::ActiveOtherListeners { listen } => json!({
                "status": "active_other_listeners",
                "port": Value::Null,
                "listen": listen,
                "hint": self.hint(),
            }),
            Self::Inactive => json!({
                "status": "inactive",
                "port": Value::Null,
                "listen": Value::Null,
                "hint": self.hint(),
            }),
            Self::NotInstalled => json!({
                "status": "not_installed",
                "port": Value::Null,
                "listen": Value::Null,
                "hint": self.hint(),
            }),
        }
    }
}

/// The concrete `:80` systemd unit probe (`ADR-0011` §3): fixed-`PATH` `systemctl` reads of the
/// real unit state, never a unit write.
///
/// Constructed only in the composition root (`crates/slipfork/src/main.rs`); the probe is
/// advisory and read-only.
pub struct SystemdIngressProbe;

impl SystemdIngressProbe {
    /// Run one `systemctl` read with the fixed diagnostic environment.
    fn systemctl(&self, args: &[&str]) -> Result<String, String> {
        let output = Command::new("systemctl")
            .args(args)
            .env_clear()
            .env("PATH", PROBE_PATH)
            .env("LC_ALL", "C")
            .env("LANGUAGE", "C")
            .output()
            .map_err(|err| format!("cannot run `systemctl {}`: {err}", args.join(" ")))?;
        if !output.status.success() {
            return Err(format!(
                "`systemctl {}` exited with {}: {}",
                args.join(" "),
                output.status,
                String::from_utf8_lossy(&output.stderr).trim()
            ));
        }
        Ok(String::from_utf8_lossy(&output.stdout).trim().to_owned())
    }

    /// `systemctl show -p Listen` -> the individual address strings.
    ///
    /// `Err` means the query failed or returned no `Listen=` entries: the caller must not claim
    /// it read the listener set.
    fn listen_addrs(&self) -> Result<Vec<String>, String> {
        let output = self.systemctl(&["show", "-p", "Listen", "slipforkd.socket"])?;
        let listen: Vec<String> = output
            .lines()
            .filter_map(|line| line.strip_prefix("Listen="))
            .map(|value| {
                value
                    .split_once(" (")
                    .map_or(value, |(address, _)| address)
                    .trim()
                    .to_owned()
            })
            .filter(|value| !value.is_empty())
            .collect();
        if listen.is_empty() {
            return Err(
                "`systemctl show -p Listen slipforkd.socket` returned no listeners".to_owned(),
            );
        }
        Ok(listen)
    }

    /// Report whether the live `slipforkd.socket` unit is installed/active and binds port `80`.
    pub fn probe_ingress(&self) -> IngressCheck {
        // `LoadState=not-found` is the only "no such unit" answer; any other load state means the
        // unit exists. An unreachable `systemctl` (no systemd host) is the same as not installed:
        // `doctor` must stay usable on containers and CI.
        let load_state = self
            .systemctl(&["show", "-p", "LoadState", "slipforkd.socket"])
            .ok()
            .and_then(|output| {
                output
                    .lines()
                    .find_map(|line| line.strip_prefix("LoadState="))
                    .map(str::to_owned)
            });
        if load_state.as_deref() != Some("loaded") {
            return IngressCheck::NotInstalled;
        }
        let active = self
            .systemctl(&["is-active", "slipforkd.socket"])
            .is_ok_and(|state| state == "active");
        if !active {
            return IngressCheck::Inactive;
        }
        match self.listen_addrs() {
            Ok(listen) => {
                let binds_port_80 = listen.iter().any(|address| {
                    address
                        .parse::<std::net::SocketAddr>()
                        .is_ok_and(|socket| socket.port() == 80)
                });
                if binds_port_80 {
                    IngressCheck::ActivePort80 { listen }
                } else {
                    IngressCheck::ActiveOtherListeners {
                        listen: Some(listen),
                    }
                }
            }
            Err(_) => IngressCheck::ActiveOtherListeners { listen: None },
        }
    }
}

/// The concrete pre-daemon namespace probe (`ADR-0010` §4): fixed-`PATH` `unshare` invocations
/// against the live kernel.
///
/// `bin_dir` is the directory holding the `slipfork`/`slipforkd` executables; it feeds
/// [`userns_remediation_hint`] so an AppArmor `EPERM` names the exact profile attachment.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OsHostProbe {
    bin_dir: PathBuf,
}

impl OsHostProbe {
    /// Bind the production probe to the composition root's resolved binary directory.
    pub fn new(bin_dir: impl Into<PathBuf>) -> Self {
        Self {
            bin_dir: bin_dir.into(),
        }
    }

    /// Run one `unshare` probe with the fixed diagnostic environment.
    fn run_unshare(&self, args: &[&str]) -> HostCheckOutcome {
        let output = Command::new("unshare")
            .args(args)
            .env_clear()
            .env("PATH", PROBE_PATH)
            .env("LC_ALL", "C")
            .env("LANGUAGE", "C")
            .output();
        let output = match output {
            Ok(output) => output,
            Err(err) => return spawn_failed(&err),
        };
        if output.status.success() {
            return HostCheckOutcome::Ok;
        }
        let stderr = String::from_utf8_lossy(&output.stderr).trim().to_owned();
        let detail = if stderr.is_empty() {
            format!("`unshare {}` exited with {}", args.join(" "), output.status)
        } else {
            stderr
        };
        HostCheckOutcome::Failed {
            hint: self.failure_hint(&detail),
            detail,
        }
    }

    /// The single hint mapping for a failed `unshare` probe (`ADR-0010` §4).
    pub(crate) fn failure_hint(&self, detail: &str) -> Option<String> {
        if detail.contains("Operation not permitted") {
            // Ubuntu 24.04 `kernel.apparmor_restrict_unprivileged_userns=1` looks like a Slipfork bug
            // until the `userns,` profile is installed: name the exact profile and command.
            Some(userns_remediation_hint(&self.bin_dir))
        } else if detail.contains("failed to execute ip")
            || detail.contains("ip: not found")
            || detail.contains("ip: No such file")
        {
            Some("install iproute2 (`ip`) and retry".to_owned())
        } else {
            None
        }
    }

    /// `unshare --user --map-user=<uid> --map-group=<gid> -- true`.
    pub fn probe_userns(&self, uid: u32, gid: u32) -> HostCheckOutcome {
        let map_user = format!("--map-user={uid}");
        let map_group = format!("--map-group={gid}");
        self.run_unshare(&["--user", &map_user, &map_group, "--", "true"])
    }

    /// `unshare --user ... --keep-caps --net --ipc --pid --fork -- ip link set lo up`.
    pub fn probe_netns(&self, uid: u32, gid: u32) -> HostCheckOutcome {
        let map_user = format!("--map-user={uid}");
        let map_group = format!("--map-group={gid}");
        self.run_unshare(&[
            "--user",
            &map_user,
            &map_group,
            "--keep-caps",
            "--net",
            "--ipc",
            "--pid",
            "--fork",
            "--",
            "ip",
            "link",
            "set",
            "lo",
            "up",
        ])
    }
}

/// The single spawn-failure mapping for the `unshare` probe (`ADR-0010` §4).
pub(crate) fn spawn_failed(err: &std::io::Error) -> HostCheckOutcome {
    let detail = if err.kind() == std::io::ErrorKind::NotFound {
        "`unshare` was not found on the diagnostic PATH".to_owned()
    } else {
        format!("cannot run `unshare`: {err}")
    };
    HostCheckOutcome::Failed {
        detail,
        hint: Some("install util-linux (`unshare`) and retry".to_owned()),
    }
}
