use std::path::{Path, PathBuf};

use serde_json::{Value, json};
use slipfork_engine::btrfs::mount_hint;
use slipfork_engine::btrfs::{BTRFS_SUPER_MAGIC, BtrfsProbeReport};
use slipfork_engine::userns_remediation_hint;

use super::probe::spawn_failed;
use super::report::filesystem_name;
use super::*;
use crate::ui::{Capabilities, TerminalUi};

fn plain() -> TerminalUi {
    TerminalUi::new(Capabilities {
        tty: false,
        color: false,
        verbose: false,
        json: false,
    })
}

fn verbose_plain() -> TerminalUi {
    TerminalUi::new(Capabilities {
        tty: false,
        color: false,
        verbose: true,
        json: false,
    })
}

#[test]
fn the_healthy_report_is_exactly_three_verdict_first_rows() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let mut btrfs = evidence(tmp.path(), BTRFS_SUPER_MAGIC);
    btrfs.noatime = true;
    let report = super::run(
        btrfs,
        HostFacts {
            unshare_userns: HostCheckOutcome::Ok,
            netns: NetnsCheck::Executed(HostCheckOutcome::Ok),
            ingress: IngressCheck::ActivePort80 {
                listen: vec!["127.0.0.1:80".to_owned()],
            },
        },
        1000,
        1000,
    );
    assert!(report.ok);
    assert_eq!(
        report.render_text(&plain()),
        "✓ Ready\n\
             \x20 ✓ Btrfs storage     btrfs, noatime\n\
             \x20 ✓ Linux namespaces  user namespaces + netns\n\
             \x20 ✓ Port 80 ingress   :80 active (port-less origins)\n"
    );
    // `-v` keeps the same three rows and appends the raw facts.
    let detailed = report.render_text(&verbose_plain());
    assert!(detailed.contains("  details:\n"), "{detailed}");
    assert!(detailed.contains("uid:gid: 1000:1000\n"), "{detailed}");
    assert!(
        detailed
            .lines()
            .filter(|line| line.starts_with("  ✓"))
            .count()
            == 3,
        "{detailed}"
    );
    let value = report.to_json();
    assert_eq!(value["healthy"], json!(true));
    assert_eq!(value["ingress"]["status"], json!("active"));
    assert_eq!(value["namespaces"]["unshare_userns"]["status"], json!("ok"));
}

#[test]
fn the_btrfs_row_reports_the_real_noatime_fact() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let report = super::run(
        evidence(tmp.path(), BTRFS_SUPER_MAGIC),
        healthy_facts(),
        1000,
        1000,
    );
    assert!(report.ok, "noatime is advisory and never fails the verdict");
    let text = report.render_text(&plain());
    assert!(
        text.contains("Btrfs storage     btrfs, atime enabled\n"),
        "an atime mount is reported as such, never as `noatime`: {text}"
    );
    assert!(!text.contains("btrfs, noatime"), "{text}");
}

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

    let text = report.render_text(&plain());
    assert_eq!(text.lines().next(), Some("✗ 3 issues"), "{text}");
    let mount_at = text
        .find(&format!("next: {}", mount_hint(&report.btrfs.probed_path)))
        .expect("mount hint line");
    let userns_at = text
        .find(&format!("next: {UNINSTALLED_HINT}"))
        .expect("userns next line");
    let netns_at = text
        .find(&format!("next: {IPROUTE2_HINT}"))
        .expect("netns next line");
    assert!(
        mount_at < userns_at && userns_at < netns_at,
        "btrfs -> userns -> netns order: {text}"
    );
    // Exactly three checklist rows, and the raw probe detail is `-v` only.
    let rows: Vec<&str> = text
        .lines()
        .filter(|line| line.starts_with("  ") && !line.trim_start().starts_with("next:"))
        .collect();
    assert_eq!(rows.len(), 3, "at most three rows: {text}");
    assert!(rows[0].contains("Btrfs storage"), "{text}");
    assert!(rows[1].contains("Linux namespaces"), "{text}");
    assert!(
        !text.contains("subvolume_ioctl"),
        "jargon is -v only: {text}"
    );

    let detailed = report.render_text(&verbose_plain());
    assert!(
        detailed.contains("subvolume_ioctl: skipped\n"),
        "{detailed}"
    );
    assert!(detailed.contains("netns: failed\n"), "{detailed}");
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
    assert!(report.render_text(&plain()).contains("Linux namespaces"));
    assert!(
        report
            .render_text(&verbose_plain())
            .contains("netns: skipped\n")
    );
    assert_eq!(
        report.to_json()["namespaces"]["netns"],
        json!({ "status": "skipped" })
    );

    // A failing report that produced no remedy never degrades into `✗ 0 issues`.
    let mut unremedied = super::run(
        evidence(tmp.path(), BTRFS_SUPER_MAGIC),
        healthy_facts(),
        1000,
        1000,
    );
    unremedied.ok = false;
    assert_eq!(
        unremedied.render_text(&plain()).lines().next(),
        Some("✗ not ready")
    );
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

    for key in ["healthy", "btrfs", "namespaces", "ingress", "fixes"] {
        assert!(value.get(key).is_some(), "missing `{key}`: {value}");
    }
    assert!(
        value.get("ok").is_none(),
        "the report carries one verdict key (`healthy`), never a duplicate `ok`: {value}"
    );
    assert_eq!(value["healthy"], json!(false));
    assert_eq!(value["namespaces"]["uid"], json!(1000));
    assert_eq!(value["namespaces"]["gid"], json!(1000));
    assert_eq!(value["btrfs"]["fstype"], json!("0x58465342"));
    assert_eq!(value["btrfs"]["super_magic"], json!(false));
    assert_eq!(value["btrfs"]["noatime"], json!(false));
    assert_eq!(
        value["btrfs"]["subvolume_ioctl"],
        json!({ "status": "skipped" })
    );
    assert_eq!(
        value["fixes"].as_array().expect("fixes array").len(),
        report.fixes.len()
    );

    let failed_object = value["namespaces"]["unshare_userns"]
        .as_object()
        .expect("object");
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
    let failed_object = with_hint["namespaces"]["unshare_userns"]
        .as_object()
        .expect("object");
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
        value["btrfs"]["subvolume_ioctl"]["status"],
        json!("failed"),
        "{value}"
    );
    assert_eq!(value["btrfs"]["subvolume_ioctl"]["errno"], json!(1));
    assert_eq!(
        value["btrfs"]["subvolume_ioctl"]["hint"],
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
    assert_eq!(report.render_text(&plain()).lines().next(), Some("✓ Ready"));
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

    let text = report.render_text(&plain());
    let label = report.ingress.label();
    assert!(text.contains(&label), "{text}");
    let btrfs_at = text.find("Btrfs storage").expect("btrfs row");
    let namespaces_at = text.find("Linux namespaces").expect("namespaces row");
    let ingress_at = text.find("Port 80 ingress").expect("ingress row");
    let fix_at = text.find("next: ").expect("mount hint line");
    assert!(
        btrfs_at < namespaces_at && namespaces_at < ingress_at && ingress_at < fix_at,
        "the three rows precede next remedies: {text}"
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
