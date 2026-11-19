//! `slipfork doctor`: host capability report, the one verb that never dials the daemon.

use std::process::ExitCode;

use slipfork::doctor::{HostCheckOutcome, HostFacts, NetnsCheck, OsHostProbe, SystemdIngressProbe};
use slipfork_engine::btrfs::KernelBtrfsDriver;

use crate::CliApp;
use crate::commands::render_json;

/// Probe the Btrfs substrate and the unprivileged namespaces without a daemon.
///
/// The CLI is the only place that constructs and observes the production probes
/// (`KernelBtrfsDriver` + `OsHostProbe` + `SystemdIngressProbe`) and then hands their plain
/// [`HostFacts`] to `doctor::run`, which spawns nothing and holds no `WorldEngine`; this works on a
/// host where `slipforkd` refuses to boot. The report renders `state_dir`, `filesystem`,
/// `btrfs_super_magic: ok|mismatch`, `subvolume_ioctl: ok|skipped|failed`, advisory `noatime`,
/// `unshare_userns`, `netns`, and the deterministic `fixes` list (`btrfs` -> `unshare_userns` ->
/// `netns`); `--json` projects the same fields, and the exit status is 0 iff the report is `ok`.
pub(crate) fn doctor(app: &CliApp) -> Result<ExitCode, String> {
    let (uid, gid) = app.ui.uid_gid();
    let driver = KernelBtrfsDriver::new();
    let host = OsHostProbe::new(app.ui.exe_dir()?);
    if !app.output_mode.json && app.ui.stdout_tty {
        eprintln!("probing namespaces and :80 ingress…");
    }
    let btrfs = driver
        .probe(app.layout.state_dir())
        .map_err(|err| err.to_string())?;
    let unshare_userns = host.probe_userns(uid, gid);
    let netns = if unshare_userns == HostCheckOutcome::Ok {
        NetnsCheck::Executed(host.probe_netns(uid, gid))
    } else {
        NetnsCheck::Skipped
    };
    let facts = HostFacts {
        unshare_userns,
        netns,
        ingress: SystemdIngressProbe.probe_ingress(),
    };
    let report = slipfork::doctor::run(btrfs, facts, uid, gid);
    if app.output_mode.json {
        println!("{}", render_json(&report.to_json())?);
    } else {
        print!("{}", report.render_text());
    }
    Ok(if report.ok {
        ExitCode::SUCCESS
    } else {
        ExitCode::from(1)
    })
}
