//! `slipfork doctor`: the pre-daemon host diagnostic seam (`ADR-0010` §4, `M3.25`).
//!
//! The CLI composition root (`crates/slipfork/src/main.rs`) constructs the two concrete probes
//! — `KernelBtrfsDriver` for the Btrfs substrate and [`OsHostProbe`] for the namespace checks —
//! observes them once, and passes plain [`HostFacts`] here. This module therefore holds **no**
//! `WorldEngine` and spawns nothing in [`run`]: `doctor` still works when `slipforkd` refuses to
//! boot on a non-Btrfs `<state_dir>`.
//!
//! [`run`] is the single composer, `filesystem_name` is the single `statfs`-code renderer,
//! [`DoctorReport::render_text`] is the single human renderer, and [`DoctorReport::to_json`] is the
//! single `--json` projection. Static `crun` is a world-rootfs asset (`ADR-0009` §1), so `doctor`
//! performs no host `crun` probe and emits no `crun` field (`ADR-0010` §4).

#![forbid(unsafe_code)]

use std::path::{Path, PathBuf};
use std::process::Command;

use serde_json::{Value, json};
use slipfork_engine::btrfs::{BtrfsProbeReport, mount_hint};
use slipfork_engine::userns_remediation_hint;

/// Fixed `PATH` every `doctor` subprocess runs with (never the caller's environment).
const PROBE_PATH: &str = "/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin";

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
    fn hint(&self) -> Option<&'static str> {
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

/// The single `slipfork doctor` report (`M3.25`, rendered by [`DoctorReport::render_text`] and
/// projected by [`DoctorReport::to_json`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DoctorReport {
    /// `true` iff every required check passed (`fixes` is empty exactly when `ok` is `true`).
    pub ok: bool,
    /// Invoking developer's uid.
    pub uid: u32,
    /// Invoking developer's gid.
    pub gid: u32,
    /// Btrfs evidence from the composition root's `statfs` probe (`ADR-0010` §4).
    pub btrfs: BtrfsProbeReport,
    /// Filesystem name derived once from `btrfs.fstype`.
    pub filesystem: String,
    /// Unprivileged user-namespace probe.
    pub unshare_userns: HostCheckOutcome,
    /// Network-namespace probe (`Skipped` when user namespaces already failed).
    pub netns: NetnsCheck,
    /// Advisory `:80` systemd socket-activation state (never feeds `ok` or `fixes`).
    pub ingress: IngressCheck,
    /// Deterministic remediation list, emitted `btrfs` -> `unshare_userns` -> `netns`.
    pub fixes: Vec<String>,
}

/// Already-observed host facts handed to [`run`] (`ADR-0010` §4).
///
/// Plain values, never adapters: [`run`] spawns no subprocess and reads no ambient state, so the
/// report is a pure composition of what the composition root observed once.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HostFacts {
    /// Result of `unshare --user`.
    pub unshare_userns: HostCheckOutcome,
    /// Result of the `netns` probe, or [`NetnsCheck::Skipped`] when user namespaces already failed.
    pub netns: NetnsCheck,
    /// Advisory `:80` systemd socket-activation state.
    pub ingress: IngressCheck,
}

/// The concrete `:80` systemd unit probe (`ADR-0011` §3): fixed-`PATH` `systemctl` reads of the
/// real unit state, never a unit write.
///
/// Constructed only in the composition root (`crates/slipfork/src/main.rs`); the probe is
/// advisory and read-only.
#[derive(Debug, Clone, Copy)]
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
    fn failure_hint(&self, detail: &str) -> Option<String> {
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
fn spawn_failed(err: &std::io::Error) -> HostCheckOutcome {
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

/// Btrfs magic and the live subvolume ioctl are both required (`ADR-0010` §4).
fn required_btrfs_ok(btrfs: &BtrfsProbeReport) -> bool {
    btrfs.btrfs_super_magic && matches!(btrfs.user_subvol_rm_allowed, Some(Ok(())))
}

/// The one remediation line for a failed required check: its hint, else its captured detail.
///
/// This keeps `fixes.is_empty() <=> ok` true even when a probe fails without a hint.
fn failed_fix(context: &str, outcome: &HostCheckOutcome) -> Option<String> {
    match outcome {
        HostCheckOutcome::Ok => None,
        HostCheckOutcome::Failed { detail, hint } => Some(
            hint.clone()
                .unwrap_or_else(|| format!("{context}: {detail}")),
        ),
    }
}

/// Compose the single [`DoctorReport`] from the composition root's already-observed facts
/// (`ADR-0010` §4).
///
/// This function probes nothing and spawns nothing: `btrfs` is the plain [`BtrfsProbeReport`] the
/// root got from `KernelBtrfsDriver::probe`, and `facts` is what [`OsHostProbe`] and
/// [`SystemdIngressProbe`] observed once in `crates/slipfork/src/main.rs`.
///
/// `NetnsCheck::Skipped` arrives in `facts` already synthesized by the root; `noatime` and
/// `ingress` are advisory and never feed `ok` or `fixes`, so `fixes.is_empty() <=> ok` holds
/// unconditionally.
pub fn run(btrfs: BtrfsProbeReport, facts: HostFacts, uid: u32, gid: u32) -> DoctorReport {
    let HostFacts {
        unshare_userns,
        netns,
        ingress,
    } = facts;
    let filesystem = filesystem_name(btrfs.fstype);

    let mut fixes = Vec::new();
    if !required_btrfs_ok(&btrfs) {
        fixes.push(mount_hint(&btrfs.probed_path));
    }
    if let Some(fix) = failed_fix("unshare_userns", &unshare_userns) {
        fixes.push(fix);
    }
    if let NetnsCheck::Executed(outcome) = &netns
        && let Some(fix) = failed_fix("netns", outcome)
    {
        fixes.push(fix);
    }

    let ok = required_btrfs_ok(&btrfs)
        && unshare_userns == HostCheckOutcome::Ok
        && netns == NetnsCheck::Executed(HostCheckOutcome::Ok);

    DoctorReport {
        ok,
        uid,
        gid,
        btrfs,
        filesystem,
        unshare_userns,
        netns,
        ingress,
        fixes,
    }
}

/// Single `statfs(2).f_type` -> filesystem name renderer (`ADR-0010` §4).
fn filesystem_name(fstype: i64) -> String {
    match fstype {
        0x9123_683e => "btrfs".to_owned(),
        0x5846_5342 => "xfs".to_owned(),
        0xef53 => "ext4".to_owned(),
        0x0102_1994 => "tmpfs".to_owned(),
        0x794c_7630 => "overlayfs".to_owned(),
        other => format!("unknown(0x{:x})", other as u32),
    }
}

impl DoctorReport {
    /// Render the human report (`ADR-0010` §4 field order, then one `fix:` line per remediation).
    pub fn render_text(&self) -> String {
        let mut text = String::new();
        text.push_str(&format!("state_dir: {}\n", self.btrfs.state_dir.display()));
        text.push_str(&format!("filesystem: {}\n", self.filesystem));
        text.push_str(&format!(
            "btrfs_super_magic: {}\n",
            if self.btrfs.btrfs_super_magic {
                "ok"
            } else {
                "mismatch"
            }
        ));
        text.push_str(&format!(
            "subvolume_ioctl: {}\n",
            match self.btrfs.user_subvol_rm_allowed {
                None => "skipped".to_owned(),
                Some(Ok(())) => "ok".to_owned(),
                Some(Err(errno)) => format!("failed (errno {errno})"),
            }
        ));
        text.push_str(&format!(
            "noatime: {}\n",
            if self.btrfs.noatime { "ok" } else { "missing" }
        ));
        text.push_str(&format!(
            "unshare_userns: {}\n",
            check_label(&self.unshare_userns)
        ));
        text.push_str(&format!("netns: {}\n", netns_label(&self.netns)));
        text.push_str(&format!("ingress: {}\n", self.ingress.label()));
        for fix in &self.fixes {
            text.push_str(&format!("fix: {fix}\n"));
        }
        text
    }

    /// Project the report into the single `--json` shape (`ADR-0010` §4).
    pub fn to_json(&self) -> Value {
        json!({
            "ok": self.ok,
            "uid": self.uid,
            "gid": self.gid,
            "state_dir": self.btrfs.state_dir,
            "probed_path": self.btrfs.probed_path,
            "fstype": format!("0x{:x}", self.btrfs.fstype as u32),
            "filesystem": self.filesystem,
            "btrfs_super_magic": self.btrfs.btrfs_super_magic,
            "subvolume_ioctl": subvolume_ioctl_json(self.btrfs.user_subvol_rm_allowed, &self.btrfs.probed_path),
            "noatime": self.btrfs.noatime,
            "unshare_userns": check_json(&self.unshare_userns),
            "netns": netns_json(&self.netns),
            "ingress": self.ingress.to_json(),
            "fixes": self.fixes,
        })
    }
}

/// `unshare_userns` text label (`ok` | `failed`); the subprocess detail stays in `--json`.
fn check_label(outcome: &HostCheckOutcome) -> String {
    match outcome {
        HostCheckOutcome::Ok => "ok".to_owned(),
        HostCheckOutcome::Failed { .. } => "failed".to_owned(),
    }
}

/// `netns` text label (`ok` | `skipped` | `failed`).
fn netns_label(netns: &NetnsCheck) -> String {
    match netns {
        NetnsCheck::Executed(outcome) => check_label(outcome),
        NetnsCheck::Skipped => "skipped".to_owned(),
    }
}

/// `subvolume_ioctl` `--json` projection (`ADR-0010` §4).
fn subvolume_ioctl_json(outcome: Option<Result<(), i32>>, probed_path: &Path) -> Value {
    match outcome {
        None => json!({ "status": "skipped" }),
        Some(Ok(())) => json!({ "status": "ok" }),
        Some(Err(errno)) => json!({
            "status": "failed",
            "errno": errno,
            "hint": mount_hint(probed_path),
        }),
    }
}

/// `unshare_userns` `--json` projection (`ADR-0010` §4).
fn check_json(outcome: &HostCheckOutcome) -> Value {
    match outcome {
        HostCheckOutcome::Ok => json!({ "status": "ok" }),
        HostCheckOutcome::Failed { detail, hint } => {
            let mut object = serde_json::Map::new();
            object.insert("status".to_owned(), json!("failed"));
            object.insert("detail".to_owned(), json!(detail));
            if let Some(hint) = hint {
                object.insert("hint".to_owned(), json!(hint));
            }
            Value::Object(object)
        }
    }
}

/// `netns` `--json` projection (`ADR-0010` §4).
fn netns_json(netns: &NetnsCheck) -> Value {
    match netns {
        NetnsCheck::Executed(outcome) => check_json(outcome),
        NetnsCheck::Skipped => json!({ "status": "skipped" }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use slipfork_engine::btrfs::{BTRFS_SUPER_MAGIC, BtrfsProbeReport};

    const XFS_SUPER_MAGIC: i64 = 0x5846_5342;
    const EXT4_SUPER_MAGIC: i64 = 0xef53;
    const UNINSTALLED_HINT: &str = "install util-linux (`unshare`) and retry";
    const IPROUTE2_HINT: &str = "install iproute2 (`ip`) and retry";

    /// The plain Btrfs evidence the composition root's one `statfs` probe produced.
    fn evidence(state_dir: &Path, fstype: i64) -> BtrfsProbeReport {
        let btrfs = fstype == BTRFS_SUPER_MAGIC;
        BtrfsProbeReport {
            state_dir: state_dir.to_path_buf(),
            probed_path: state_dir.to_path_buf(),
            fstype,
            btrfs_super_magic: btrfs,
            user_subvol_rm_allowed: btrfs.then_some(Ok(())),
            noatime: false,
        }
    }

    /// Deterministic facts the composition root would have observed on a healthy host.
    fn healthy_facts() -> HostFacts {
        HostFacts {
            unshare_userns: HostCheckOutcome::Ok,
            netns: NetnsCheck::Executed(HostCheckOutcome::Ok),
            ingress: IngressCheck::NotInstalled,
        }
    }

    fn failed(detail: &str, hint: Option<&str>) -> HostCheckOutcome {
        HostCheckOutcome::Failed {
            detail: detail.to_owned(),
            hint: hint.map(str::to_owned),
        }
    }

    #[test]
    fn non_btrfs_evidence_leads_with_the_mount_hint_and_creates_nothing() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let state_dir = tmp.path().join("missing-state");
        let report = super::run(
            evidence(&state_dir, XFS_SUPER_MAGIC),
            healthy_facts(),
            1000,
            1000,
        );

        assert!(!report.ok);
        assert!(!report.btrfs.btrfs_super_magic);
        assert_eq!(report.filesystem, "xfs");
        assert_eq!(report.fixes, vec![mount_hint(&report.btrfs.probed_path)]);
        assert!(!state_dir.exists());
    }

    #[test]
    fn fixes_follow_btrfs_then_userns_then_netns_with_literal_remedies() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let report = super::run(
            evidence(&tmp.path().join("state"), EXT4_SUPER_MAGIC),
            HostFacts {
                unshare_userns: failed("Operation not permitted", Some(UNINSTALLED_HINT)),
                netns: NetnsCheck::Executed(failed("ip: not found", Some(IPROUTE2_HINT))),
                ingress: IngressCheck::NotInstalled,
            },
            1000,
            1000,
        );

        assert!(!report.ok);
        assert_eq!(
            report.fixes,
            vec![
                mount_hint(&report.btrfs.probed_path),
                UNINSTALLED_HINT.to_owned(),
                IPROUTE2_HINT.to_owned(),
            ]
        );

        let text = report.render_text();
        let mount_at = text
            .find(&format!("fix: {}", mount_hint(&report.btrfs.probed_path)))
            .expect("mount hint line");
        let userns_at = text
            .find(&format!("fix: {UNINSTALLED_HINT}"))
            .expect("userns fix line");
        let netns_at = text
            .find(&format!("fix: {IPROUTE2_HINT}"))
            .expect("netns fix line");
        assert!(
            mount_at < userns_at && userns_at < netns_at,
            "btrfs -> userns -> netns order: {text}"
        );
        assert!(text.contains("subvolume_ioctl: skipped\n"), "{text}");
        assert!(text.contains("netns: failed\n"), "{text}");
    }

    #[test]
    fn skipped_netns_renders_without_a_netns_fix() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let report = super::run(
            evidence(tmp.path(), BTRFS_SUPER_MAGIC),
            HostFacts {
                unshare_userns: failed("Operation not permitted", Some(UNINSTALLED_HINT)),
                netns: NetnsCheck::Skipped,
                ingress: IngressCheck::NotInstalled,
            },
            1000,
            1000,
        );

        assert!(!report.ok);
        assert_eq!(report.fixes, vec![UNINSTALLED_HINT.to_owned()]);
        assert!(report.render_text().contains("netns: skipped\n"));
        assert_eq!(report.to_json()["netns"], json!({ "status": "skipped" }));
    }

    #[test]
    fn json_projection_carries_every_key_and_the_failed_variant() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let report = super::run(
            evidence(&tmp.path().join("state"), XFS_SUPER_MAGIC),
            HostFacts {
                unshare_userns: failed("unshare: Operation not permitted", None),
                netns: NetnsCheck::Skipped,
                ingress: IngressCheck::NotInstalled,
            },
            1000,
            1000,
        );
        let value = report.to_json();

        for key in [
            "ok",
            "uid",
            "gid",
            "state_dir",
            "probed_path",
            "fstype",
            "filesystem",
            "btrfs_super_magic",
            "subvolume_ioctl",
            "noatime",
            "unshare_userns",
            "netns",
            "ingress",
            "fixes",
        ] {
            assert!(value.get(key).is_some(), "missing `{key}`: {value}");
        }
        assert_eq!(value["uid"], json!(1000));
        assert_eq!(value["gid"], json!(1000));
        assert_eq!(value["fstype"], json!("0x58465342"));
        assert_eq!(value["btrfs_super_magic"], json!(false));
        assert_eq!(value["noatime"], json!(false));
        assert_eq!(value["subvolume_ioctl"], json!({ "status": "skipped" }));
        assert_eq!(
            value["fixes"].as_array().expect("fixes array").len(),
            report.fixes.len()
        );

        let failed_object = value["unshare_userns"].as_object().expect("object");
        let mut keys: Vec<&str> = failed_object.keys().map(String::as_str).collect();
        keys.sort_unstable();
        assert_eq!(keys, ["detail", "status"]);
        assert_eq!(failed_object["status"], json!("failed"));
        assert_eq!(
            failed_object["detail"],
            json!("unshare: Operation not permitted")
        );
        assert!(value.get("crun").is_none(), "no crun field");

        let with_hint = super::run(
            evidence(tmp.path(), BTRFS_SUPER_MAGIC),
            HostFacts {
                unshare_userns: failed("denied", Some("install the profile")),
                netns: NetnsCheck::Skipped,
                ingress: IngressCheck::NotInstalled,
            },
            1000,
            1000,
        )
        .to_json();
        let failed_object = with_hint["unshare_userns"].as_object().expect("object");
        let mut keys: Vec<&str> = failed_object.keys().map(String::as_str).collect();
        keys.sort_unstable();
        assert_eq!(keys, ["detail", "hint", "status"]);
        assert_eq!(failed_object["hint"], json!("install the profile"));
    }

    #[test]
    fn failed_ioctl_evidence_carries_the_errno_and_mount_hint() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let mut btrfs = evidence(tmp.path(), BTRFS_SUPER_MAGIC);
        btrfs.user_subvol_rm_allowed = Some(Err(1));
        let report = super::run(btrfs, healthy_facts(), 1000, 1000);
        let value = report.to_json();

        assert!(!report.ok);
        assert_eq!(
            value["subvolume_ioctl"]["status"],
            json!("failed"),
            "{value}"
        );
        assert_eq!(value["subvolume_ioctl"]["errno"], json!(1));
        assert_eq!(
            value["subvolume_ioctl"]["hint"],
            json!(mount_hint(&report.btrfs.probed_path))
        );
        assert_eq!(report.fixes, vec![mount_hint(&report.btrfs.probed_path)]);
    }

    #[test]
    fn btrfs_evidence_passes_with_an_empty_fix_list() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let report = super::run(
            evidence(tmp.path(), BTRFS_SUPER_MAGIC),
            healthy_facts(),
            1000,
            1000,
        );

        assert!(report.ok);
        assert!(report.btrfs.btrfs_super_magic);
        assert_eq!(report.btrfs.user_subvol_rm_allowed, Some(Ok(())));
        assert_eq!(report.filesystem, "btrfs");
        assert!(report.fixes.is_empty());
        assert_eq!(report.netns, NetnsCheck::Executed(HostCheckOutcome::Ok));
    }

    #[test]
    fn filesystem_names_cover_the_documented_codes() {
        assert_eq!(filesystem_name(0x9123_683e), "btrfs");
        assert_eq!(filesystem_name(0x5846_5342), "xfs");
        assert_eq!(filesystem_name(0xef53), "ext4");
        assert_eq!(filesystem_name(0x0102_1994), "tmpfs");
        assert_eq!(filesystem_name(0x794c_7630), "overlayfs");
        assert_eq!(filesystem_name(0x1), "unknown(0x1)");
    }

    #[test]
    fn live_ingress_matches_the_concrete_probe_and_never_feeds_the_verdict() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let probe = SystemdIngressProbe;
        let report = super::run(
            evidence(&tmp.path().join("state"), EXT4_SUPER_MAGIC),
            HostFacts {
                unshare_userns: HostCheckOutcome::Ok,
                netns: NetnsCheck::Executed(HostCheckOutcome::Ok),
                ingress: probe.probe_ingress(),
            },
            1000,
            1000,
        );

        assert_eq!(report.ingress, probe.probe_ingress());
        assert!(!report.ok, "ingress never makes a non-Btrfs host healthy");
        assert_eq!(report.fixes, vec![mount_hint(&report.btrfs.probed_path)]);

        let text = report.render_text();
        let label = report.ingress.label();
        assert!(text.contains(&format!("ingress: {label}\n")), "{text}");
        let userns_at = text.find("unshare_userns: ").expect("userns line");
        let ingress_at = text.find("ingress: ").expect("ingress line");
        let fix_at = text.find("fix: ").expect("mount hint line");
        assert!(
            userns_at < ingress_at && ingress_at < fix_at,
            "fatal probes precede the advisory ingress line, which precedes fixes: {text}"
        );

        let value = report.to_json();
        for key in ["port", "listen", "hint"] {
            assert!(
                value["ingress"].get(key).is_some(),
                "ingress.{key} must always be present: {value}"
            );
        }
    }

    #[test]
    fn every_ingress_state_has_its_design_text_and_typed_json() {
        for (check, label, status) in [
            (
                IngressCheck::NotInstalled,
                "not installed (ephemeral port; run `sudo slipfork daemon install`)",
                "not_installed",
            ),
            (
                IngressCheck::Inactive,
                "installed but inactive (run `sudo systemctl start slipforkd.socket`)",
                "inactive",
            ),
            (
                IngressCheck::ActiveOtherListeners {
                    listen: Some(vec!["127.0.0.1:7080".to_owned()]),
                },
                "active without :80 (origins carry the answered port; run `sudo slipfork daemon install` to rebind :80)",
                "active_other_listeners",
            ),
            (
                IngressCheck::ActiveOtherListeners { listen: None },
                "active but `systemctl show -p Listen slipforkd.socket` failed; cannot confirm :80 (run `sudo systemctl show -p Listen slipforkd.socket`)",
                "active_other_listeners",
            ),
        ] {
            assert_eq!(check.label(), label);
            assert_eq!(check.hint(), Some(label));
            let value = check.to_json();
            assert_eq!(value["status"], json!(status));
            assert_eq!(value["port"], Value::Null);
            assert_eq!(value["hint"], json!(label));
            assert!(
                value["listen"].is_null() || value["listen"].is_array(),
                "listen is a list or null: {value}"
            );
        }

        let active = IngressCheck::ActivePort80 {
            listen: vec!["127.0.0.1:80".to_owned(), "[::1]:80".to_owned()],
        };
        assert_eq!(active.hint(), None);
        assert_eq!(active.label(), ":80 active (port-less origins)");
        let value = active.to_json();
        assert_eq!(value["status"], json!("active"));
        assert_eq!(value["port"], json!(80));
        assert_eq!(value["listen"], json!(["127.0.0.1:80", "[::1]:80"]));
        assert_eq!(value["hint"], Value::Null);
    }

    #[test]
    fn unshare_failure_paths_map_to_the_literal_remedies() {
        let probe = OsHostProbe::new("/usr/bin");
        assert_eq!(
            probe.failure_hint("unshare: Operation not permitted"),
            Some(userns_remediation_hint(&PathBuf::from("/usr/bin")))
        );
        assert_eq!(
            probe.failure_hint("unshare: failed to execute ip: No such file or directory"),
            Some(IPROUTE2_HINT.to_owned())
        );
        assert_eq!(probe.failure_hint("unshare: some other denial"), None);

        assert_eq!(
            spawn_failed(&std::io::Error::from(std::io::ErrorKind::NotFound)),
            failed(
                "`unshare` was not found on the diagnostic PATH",
                Some(UNINSTALLED_HINT)
            )
        );
        let other = std::io::Error::from(std::io::ErrorKind::PermissionDenied);
        assert_eq!(
            spawn_failed(&other),
            failed(
                &format!("cannot run `unshare`: {other}"),
                Some(UNINSTALLED_HINT)
            )
        );
    }
}
