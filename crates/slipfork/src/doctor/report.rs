//! The `slipfork doctor` report: one composer, one human renderer, one JSON projection.

use std::path::Path;

use serde_json::{Value, json};
use slipfork_engine::btrfs::{BtrfsProbeReport, mount_hint};

use crate::ui::{TerminalUi, column_width};

use super::probe::{HostCheckOutcome, IngressCheck, NetnsCheck};

/// The single `slipfork doctor` report (`M3.25`, rendered by [`DoctorReport::render_text`] and
/// projected by [`DoctorReport::to_json`]).
///
/// One composer, one human renderer, one JSON projection.
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

/// Btrfs magic and the live subvolume ioctl are both required (`ADR-0010` §4).
pub(crate) fn required_btrfs_ok(btrfs: &BtrfsProbeReport) -> bool {
    btrfs.btrfs_super_magic && matches!(btrfs.user_subvol_rm_allowed, Some(Ok(())))
}

/// The one remediation line for a failed required check: its hint, else its captured detail.
///
/// This keeps `fixes.is_empty() <=> ok` true even when a probe fails without a hint.
pub(crate) fn failed_fix(context: &str, outcome: &HostCheckOutcome) -> Option<String> {
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
/// root got from `KernelBtrfsDriver::probe`, and `facts` is what `OsHostProbe` and
/// `SystemdIngressProbe` observed once in `crates/slipfork/src/main.rs`.
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

    let mut report = DoctorReport {
        ok: false,
        uid,
        gid,
        btrfs,
        filesystem,
        unshare_userns,
        netns,
        ingress,
        fixes,
    };
    report.ok = report.btrfs_ok() && report.namespaces_ok();
    report
}

/// Single `statfs(2).f_type` -> filesystem name renderer (`ADR-0010` §4).
pub(crate) fn filesystem_name(fstype: i64) -> String {
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
    /// `true` iff the Btrfs magic and the live subvolume ioctl both passed (`ADR-0010` §4).
    pub fn btrfs_ok(&self) -> bool {
        required_btrfs_ok(&self.btrfs)
    }

    /// `true` iff unprivileged user namespaces and `netns` both ran successfully.
    ///
    /// `NetnsCheck::Skipped` is never success: the shell could not be created at all.
    pub fn namespaces_ok(&self) -> bool {
        self.unshare_userns == HostCheckOutcome::Ok
            && self.netns == NetnsCheck::Executed(HostCheckOutcome::Ok)
    }

    /// Render the human report: the verdict, exactly three checklist rows, then one `next:` per fix.
    ///
    /// The three rows are Btrfs storage, Linux namespaces, and the `:80` ingress (`docs/design/
    /// dx-cli-0.1.0.md` §2). `-v` appends a raw-facts block (state dir, fstype, uid/gid, probe
    /// detail); a default run never prints that jargon.
    pub fn render_text(&self, ui: &TerminalUi) -> String {
        let rows = self.checklist(ui);
        let label_width = column_width(0, rows.iter().map(|row| row.label));
        let mut text = String::new();
        text.push_str(&self.verdict(ui));
        text.push('\n');
        for row in &rows {
            text.push_str(&format!(
                "  {} {}  {}\n",
                row.mark,
                ui.pad(row.label, label_width),
                row.detail
            ));
        }
        if ui.verbose() {
            text.push_str(&self.raw_details(ui));
        }
        for fix in &self.fixes {
            text.push_str(&format!("  next: {fix}\n"));
        }
        text
    }

    /// The three checklist rows, each with its own mark and one-line detail.
    fn checklist(&self, ui: &TerminalUi) -> [ChecklistRow; 3] {
        let btrfs_ok = self.btrfs_ok();
        let namespaces_ok = self.namespaces_ok();
        let ingress_ok = matches!(self.ingress, IngressCheck::ActivePort80 { .. });
        [
            ChecklistRow {
                mark: mark(ui, btrfs_ok),
                label: "Btrfs storage",
                detail: if !btrfs_ok {
                    format!("{}, needs btrfs", self.filesystem)
                } else if self.btrfs.noatime {
                    format!("{}, noatime", self.filesystem)
                } else {
                    format!("{}, atime enabled", self.filesystem)
                },
            },
            ChecklistRow {
                mark: mark(ui, namespaces_ok),
                label: "Linux namespaces",
                detail: match (&self.unshare_userns, &self.netns) {
                    (HostCheckOutcome::Ok, NetnsCheck::Executed(HostCheckOutcome::Ok)) => {
                        "user namespaces + netns".to_owned()
                    }
                    (HostCheckOutcome::Ok, NetnsCheck::Skipped) => {
                        "netns skipped after a userns failure".to_owned()
                    }
                    (HostCheckOutcome::Ok, _) => "netns unavailable".to_owned(),
                    (_, NetnsCheck::Skipped) => {
                        "user namespaces unavailable (netns skipped)".to_owned()
                    }
                    _ => "user namespaces unavailable".to_owned(),
                },
            },
            ChecklistRow {
                mark: if ingress_ok {
                    ui.ok("✓")
                } else {
                    ui.warn("!")
                },
                label: "Port 80 ingress",
                detail: self.ingress.label(),
            },
        ]
    }

    /// The `-v` raw-facts block: mount, fstype, uid/gid, and every probe's own verdict.
    fn raw_details(&self, ui: &TerminalUi) -> String {
        let mut text = String::from("  details:\n");
        let rows = [
            format!("state_dir: {}", self.btrfs.state_dir.display()),
            format!(
                "filesystem: {} (0x{:x})",
                self.filesystem, self.btrfs.fstype as u32
            ),
            format!(
                "btrfs_super_magic: {}",
                if self.btrfs.btrfs_super_magic {
                    "ok"
                } else {
                    "mismatch"
                }
            ),
            format!(
                "subvolume_ioctl: {}",
                match self.btrfs.user_subvol_rm_allowed {
                    None => "skipped".to_owned(),
                    Some(Ok(())) => "ok".to_owned(),
                    Some(Err(errno)) => format!("failed (errno {errno})"),
                }
            ),
            format!("unshare_userns: {}", check_label(&self.unshare_userns, ui)),
            format!("netns: {}", netns_label(&self.netns, ui)),
            format!("uid:gid: {}:{}", self.uid, self.gid),
            format!("ingress: {}", self.ingress.label()),
        ];
        for row in rows {
            text.push_str(&format!("    {}\n", ui.secondary(&row)));
        }
        text
    }

    /// The one-line verdict `doctor` leads with: `✓ Ready` or `✗ N issue(s)`.
    fn verdict(&self, ui: &TerminalUi) -> String {
        if self.ok {
            return ui.ok("✓ Ready");
        }
        let count = self.fixes.len();
        ui.failed(&match count {
            0 => "✗ not ready".to_owned(),
            1 => "✗ 1 issue".to_owned(),
            count => format!("✗ {count} issues"),
        })
    }

    /// Project the report into the single `--json` shape (`docs/design/dx-cli-0.1.0.md` §3).
    pub fn to_json(&self) -> Value {
        json!({
            "healthy": self.ok,
            "btrfs": {
                "state_dir": self.btrfs.state_dir,
                "probed_path": self.btrfs.probed_path,
                "fstype": format!("0x{:x}", self.btrfs.fstype as u32),
                "filesystem": self.filesystem,
                "super_magic": self.btrfs.btrfs_super_magic,
                "subvolume_ioctl": subvolume_ioctl_json(self.btrfs.user_subvol_rm_allowed, &self.btrfs.probed_path),
                "noatime": self.btrfs.noatime,
            },
            "namespaces": {
                "uid": self.uid,
                "gid": self.gid,
                "unshare_userns": check_json(&self.unshare_userns),
                "netns": netns_json(&self.netns),
            },
            "ingress": self.ingress.to_json(),
            "fixes": self.fixes,
        })
    }
}

/// One checklist row: the colored mark, the fixed label, and the one-line detail.
struct ChecklistRow {
    mark: String,
    label: &'static str,
    detail: String,
}

/// The shared `✓` / `✗` mark for a required check.
fn mark(ui: &TerminalUi, ok: bool) -> String {
    if ok { ui.ok("✓") } else { ui.failed("✗") }
}

/// `unshare_userns` text label (`ok` | `failed`); the subprocess detail stays in `--json`.
fn check_label(outcome: &HostCheckOutcome, ui: &TerminalUi) -> String {
    match outcome {
        HostCheckOutcome::Ok => ui.ok("ok"),
        HostCheckOutcome::Failed { .. } => ui.failed("failed"),
    }
}

/// `netns` text label (`ok` | `skipped` | `failed`).
fn netns_label(netns: &NetnsCheck, ui: &TerminalUi) -> String {
    match netns {
        NetnsCheck::Executed(outcome) => check_label(outcome, ui),
        NetnsCheck::Skipped => ui.warn("skipped"),
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
