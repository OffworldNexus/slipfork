//! `slipfork doctor`: the pre-daemon host diagnostic seam (`ADR-0010` §4, `M3.25`).
//!
//! The CLI composition root (`crates/slipfork/src/main.rs`) constructs the two production probes
//! — `KernelBtrfsDriver` for the Btrfs substrate and [`OsHostProbe`] for the namespace checks —
//! and injects them here. This module therefore holds **no** `WorldEngine`: `doctor` still works
//! when `slipforkd` refuses to boot on a non-Btrfs `<state_dir>`.
//!
//! [`run`] is the single composer, `filesystem_name` is the single `statfs`-code renderer,
//! [`DoctorReport::render_text`] is the single human renderer, and [`DoctorReport::to_json`] is the
//! single `--json` projection. Static `crun` is a world-rootfs asset (`ADR-0009` §1), so `doctor`
//! performs no host `crun` probe and emits no `crun` field (`ADR-0010` §4).

#![forbid(unsafe_code)]

use std::path::PathBuf;
use std::process::Command;

use serde_json::{Value, json};
use slipfork_engine::btrfs::{BTRFS_MOUNT_HINT, BtrfsProbeReport, SubvolumeDriver, probe_with};
use slipfork_engine::{EngineError, userns_remediation_hint};

/// Fixed `PATH` every `doctor` subprocess runs with (never the caller's environment).
const PROBE_PATH: &str = "/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin";

/// Outcome of one injected namespace probe.
///
/// Two variants only: [`NetnsCheck::Skipped`] exists exclusively on [`DoctorReport::netns`] and is
/// synthesized by [`run`], so a [`HostProbe`] implementation can never report `Skipped`.
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
/// otherwise (synthesized solely by [`run`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NetnsCheck {
    /// The `netns`/`ipcns`/`pidns` probe ran.
    Executed(HostCheckOutcome),
    /// `unshare --user` failed, so the `netns` probe would only restate the same denial.
    Skipped,
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
    /// Btrfs evidence from [`SubvolumeDriver::probe`] (`ADR-0010` §4).
    pub btrfs: BtrfsProbeReport,
    /// Filesystem name derived once from `btrfs.fstype`.
    pub filesystem: String,
    /// Unprivileged user-namespace probe.
    pub unshare_userns: HostCheckOutcome,
    /// Network-namespace probe (`Skipped` when user namespaces already failed).
    pub netns: NetnsCheck,
    /// Deterministic remediation list, emitted `btrfs` -> `unshare_userns` -> `netns`.
    pub fixes: Vec<String>,
}

/// Sole pre-daemon namespace probe seam (`ADR-0010` §4).
///
/// `OsHostProbe` is the only production adapter (constructed in `crates/slipfork/src/main.rs`);
/// tests inject fakes. Both probes run with a cleared environment, a fixed `PATH`, and `LC_ALL=C`
/// so a diagnostic never depends on the caller's shell configuration.
pub trait HostProbe: Send + Sync {
    /// `unshare --user --map-user=<uid> --map-group=<gid> -- true`.
    fn probe_userns(&self, uid: u32, gid: u32) -> HostCheckOutcome;

    /// `unshare --user ... --keep-caps --net --ipc --pid --fork -- ip link set lo up`.
    fn probe_netns(&self, uid: u32, gid: u32) -> HostCheckOutcome;
}

/// Production [`HostProbe`]: fixed-`PATH` `unshare` invocations.
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
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
                return HostCheckOutcome::Failed {
                    detail: "`unshare` was not found on the diagnostic PATH".to_owned(),
                    hint: Some("install util-linux (`unshare`) and retry".to_owned()),
                };
            }
            Err(err) => {
                return HostCheckOutcome::Failed {
                    detail: format!("cannot run `unshare`: {err}"),
                    hint: Some("install util-linux (`unshare`) and retry".to_owned()),
                };
            }
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
}

impl HostProbe for OsHostProbe {
    fn probe_userns(&self, uid: u32, gid: u32) -> HostCheckOutcome {
        let map_user = format!("--map-user={uid}");
        let map_group = format!("--map-group={gid}");
        self.run_unshare(&["--user", &map_user, &map_group, "--", "true"])
    }

    fn probe_netns(&self, uid: u32, gid: u32) -> HostCheckOutcome {
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

/// Btrfs magic and the live subvolume ioctl are both required (`ADR-0010` §4).
fn required_btrfs_ok(btrfs: &BtrfsProbeReport) -> bool {
    btrfs.btrfs_super_magic && matches!(btrfs.user_subvol_rm_allowed, Some(Ok(())))
}

/// The one remediation line for a failed required check: its hint, else its captured detail.
///
/// This keeps `fixes.is_empty() <=> ok` true even when an injected probe fails without a hint.
fn failed_fix(context: &str, outcome: &HostCheckOutcome) -> Option<String> {
    match outcome {
        HostCheckOutcome::Ok => None,
        HostCheckOutcome::Failed { detail, hint } => Some(
            hint.clone()
                .unwrap_or_else(|| format!("{context}: {detail}")),
        ),
    }
}

/// Compose the single [`DoctorReport`] from the injected probes (`ADR-0010` §4).
///
/// `NetnsCheck::Skipped` is synthesized here (and only here) when user namespaces already failed;
/// `noatime` is advisory and never feeds `ok` or `fixes`, so `fixes.is_empty() <=> ok` holds
/// unconditionally.
pub fn run(
    driver: &dyn SubvolumeDriver,
    host: &dyn HostProbe,
    state_dir: &std::path::Path,
    uid: u32,
    gid: u32,
) -> Result<DoctorReport, EngineError> {
    let btrfs = probe_with(driver, state_dir)?;
    let unshare_userns = host.probe_userns(uid, gid);
    let netns = if unshare_userns == HostCheckOutcome::Ok {
        NetnsCheck::Executed(host.probe_netns(uid, gid))
    } else {
        NetnsCheck::Skipped
    };
    let filesystem = filesystem_name(btrfs.fstype);

    let mut fixes = Vec::new();
    if !required_btrfs_ok(&btrfs) {
        fixes.push(BTRFS_MOUNT_HINT.to_owned());
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

    Ok(DoctorReport {
        ok,
        uid,
        gid,
        btrfs,
        filesystem,
        unshare_userns,
        netns,
        fixes,
    })
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
            "subvolume_ioctl": subvolume_ioctl_json(self.btrfs.user_subvol_rm_allowed),
            "noatime": self.btrfs.noatime,
            "unshare_userns": check_json(&self.unshare_userns),
            "netns": netns_json(&self.netns),
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
fn subvolume_ioctl_json(outcome: Option<Result<(), i32>>) -> Value {
    match outcome {
        None => json!({ "status": "skipped" }),
        Some(Ok(())) => json!({ "status": "ok" }),
        Some(Err(errno)) => json!({
            "status": "failed",
            "errno": errno,
            "hint": BTRFS_MOUNT_HINT,
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
    use slipfork_testkit::btrfs::TestTreeDriver;

    /// Deterministic [`HostProbe`] for the composer tests.
    struct FakeHostProbe {
        userns: HostCheckOutcome,
        netns: HostCheckOutcome,
    }

    impl FakeHostProbe {
        fn passing() -> Self {
            Self {
                userns: HostCheckOutcome::Ok,
                netns: HostCheckOutcome::Ok,
            }
        }
    }

    impl HostProbe for FakeHostProbe {
        fn probe_userns(&self, _uid: u32, _gid: u32) -> HostCheckOutcome {
            self.userns.clone()
        }

        fn probe_netns(&self, _uid: u32, _gid: u32) -> HostCheckOutcome {
            self.netns.clone()
        }
    }

    #[test]
    fn non_btrfs_reports_the_mount_hint_and_creates_nothing() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let state_dir = tmp.path().join("missing-state");
        let driver = TestTreeDriver::non_btrfs(0x5846_5342);
        let report = run(&driver, &FakeHostProbe::passing(), &state_dir, 1000, 1000)
            .expect("probe succeeds off Btrfs");

        assert!(!report.ok);
        assert!(!report.btrfs.btrfs_super_magic);
        assert_eq!(report.btrfs.user_subvol_rm_allowed, None);
        assert_eq!(report.filesystem, "xfs");
        assert_eq!(report.fixes, vec![BTRFS_MOUNT_HINT.to_owned()]);
        assert!(!state_dir.exists(), "doctor must create nothing off Btrfs");
    }

    #[test]
    fn failed_checks_without_a_hint_still_produce_a_fix() {
        let tmp = tempfile::tempdir().expect("tempdir");

        let userns_only = FakeHostProbe {
            userns: HostCheckOutcome::Failed {
                detail: "unshare: Operation not permitted".to_owned(),
                hint: None,
            },
            netns: HostCheckOutcome::Ok,
        };
        let report = run(
            &TestTreeDriver::btrfs(),
            &userns_only,
            tmp.path(),
            1000,
            1000,
        )
        .expect("probe succeeds on Btrfs");
        assert!(!report.ok);
        assert_eq!(
            report.fixes,
            vec!["unshare_userns: unshare: Operation not permitted".to_owned()]
        );
        assert_eq!(report.netns, NetnsCheck::Skipped);

        let netns_only = FakeHostProbe {
            userns: HostCheckOutcome::Ok,
            netns: HostCheckOutcome::Failed {
                detail: "ip link: Operation not permitted".to_owned(),
                hint: None,
            },
        };
        let report = run(
            &TestTreeDriver::btrfs(),
            &netns_only,
            tmp.path(),
            1000,
            1000,
        )
        .expect("probe succeeds on Btrfs");
        assert!(!report.ok);
        assert_eq!(
            report.fixes,
            vec!["netns: ip link: Operation not permitted".to_owned()]
        );
    }

    #[test]
    fn btrfs_host_passes_with_an_empty_fix_list() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let driver = TestTreeDriver::btrfs();
        let report = run(&driver, &FakeHostProbe::passing(), tmp.path(), 1000, 1000)
            .expect("probe succeeds on Btrfs");

        assert!(report.ok);
        assert!(report.btrfs.btrfs_super_magic);
        assert_eq!(report.btrfs.user_subvol_rm_allowed, Some(Ok(())));
        assert_eq!(report.filesystem, "btrfs");
        assert!(report.fixes.is_empty());
        assert_eq!(report.netns, NetnsCheck::Executed(HostCheckOutcome::Ok));
    }

    #[test]
    fn netns_is_skipped_only_when_user_namespaces_fail() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let host = FakeHostProbe {
            userns: HostCheckOutcome::Failed {
                detail: "Operation not permitted".to_owned(),
                hint: Some("install the profile".to_owned()),
            },
            netns: HostCheckOutcome::Ok,
        };
        let report = run(&TestTreeDriver::btrfs(), &host, tmp.path(), 1000, 1000)
            .expect("probe succeeds on Btrfs");

        assert!(!report.ok);
        assert_eq!(report.netns, NetnsCheck::Skipped);
        assert_eq!(report.fixes, vec!["install the profile".to_owned()]);
    }

    #[test]
    fn fixes_are_emitted_btrfs_then_userns_then_netns() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let host = FakeHostProbe {
            userns: HostCheckOutcome::Ok,
            netns: HostCheckOutcome::Failed {
                detail: "ip: not found".to_owned(),
                hint: Some("install iproute2".to_owned()),
            },
        };
        let report = run(
            &TestTreeDriver::non_btrfs(0xef53),
            &host,
            &tmp.path().join("state"),
            1000,
            1000,
        )
        .expect("probe succeeds off Btrfs");

        assert_eq!(
            report.fixes,
            vec![BTRFS_MOUNT_HINT.to_owned(), "install iproute2".to_owned()]
        );
        let text = report.render_text();
        assert!(text.contains("filesystem: ext4"), "{text}");
        assert!(text.contains("subvolume_ioctl: skipped"), "{text}");
        assert!(text.contains("netns: failed\n"), "{text}");
        assert!(text.contains("fix: install iproute2"), "{text}");
    }

    #[test]
    fn json_projection_uses_the_adr_field_names() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let report = run(
            &TestTreeDriver::non_btrfs(0x5846_5342),
            &FakeHostProbe::passing(),
            &tmp.path().join("state"),
            1000,
            1000,
        )
        .expect("probe succeeds off Btrfs");
        let value = report.to_json();

        assert_eq!(value["ok"], json!(false));
        assert_eq!(value["filesystem"], json!("xfs"));
        assert_eq!(value["fstype"], json!("0x58465342"));
        assert_eq!(value["btrfs_super_magic"], json!(false));
        assert_eq!(value["subvolume_ioctl"], json!({ "status": "skipped" }));
        assert_eq!(value["unshare_userns"], json!({ "status": "ok" }));
        assert_eq!(value["netns"], json!({ "status": "ok" }));
        assert_eq!(value["fixes"], json!([BTRFS_MOUNT_HINT]));
        assert!(
            value.get("crun").is_none(),
            "doctor never emits a crun field"
        );
    }

    #[test]
    fn failed_ioctl_json_carries_the_errno_and_mount_hint() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let driver = TestTreeDriver::btrfs().with_destroy_errno(1);
        let report = run(&driver, &FakeHostProbe::passing(), tmp.path(), 1000, 1000)
            .expect("a scratch round-trip failure is evidence, not an error");
        let value = report.to_json();

        assert_eq!(
            value["subvolume_ioctl"]["status"],
            json!("failed"),
            "{value}"
        );
        assert_eq!(value["subvolume_ioctl"]["errno"], json!(1));
        assert_eq!(value["subvolume_ioctl"]["hint"], json!(BTRFS_MOUNT_HINT));
        assert!(!report.ok);
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
}
