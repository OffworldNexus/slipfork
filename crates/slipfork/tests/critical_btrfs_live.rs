//! Protected live composition-root suite (`M3.15`, `ADR-0010` §§4–5).
//!
//! This suite spawns the **real** `CARGO_BIN_EXE_slipforkd`. It has two honest modes and never
//! skips:
//!
//! - **Btrfs host:** runs the full production path — the composition root constructs
//!   `BtrfsEngine` over `KernelBtrfsDriver`, the fixture `snow.yml` builds an `L0` rootfs, a
//!   `postgres:18` `crun` backing service seeds a clean base, and the sealed base snapshot is
//!   cloned into a live world.
//! - **Non-Btrfs host:** the same binary must fail closed with `EngineError::BtrfsRequired`: exit
//!   1, the exact `mount -o user_subvol_rm_allowed,noatime` hint, and no world/snapshot directory.
//!
//! No `#[ignore]` and no fallback path exists in either branch.

use std::fs;
use std::process::Stdio;

pub mod support;
use support::{
    Daemon, DaemonLayout, assert_btrfs_refusal, assert_success, daemon_command, disk_tempdir,
    repo_with_manifest, stderr, stdout,
};

const LIVE_SNOW: &str = r#"
images:
  - id: app
    context: .
    dockerfile: Dockerfile

components:
  - id: api
    image: app
    run_command: "exec sleep 300"

ingresses:
  - match:
      prefix: "/"
    target:
      component: api
      port: 8000

resources:
  pg_clusters:
    - id: main
      version: "18"
  pg_databases:
    - id: main
      cluster: main

modes:
  local:
    envs:
      app:
        BASE_URL: '${{ resources::app.url }}'
"#;

#[test]
fn production_slipforkd_is_btrfs_only_and_boots_a_real_world_on_btrfs() {
    let tmp = disk_tempdir("live-");
    let scratch = tmp.path();
    let layout = DaemonLayout::under(scratch);

    if !support::host_is_btrfs(&layout.state_dir) {
        // The one child-daemon argv/env builder, with its stderr captured for the refusal proof.
        let mut command = daemon_command(scratch, &layout);
        let output = command
            .stderr(Stdio::piped())
            .output()
            .expect("spawn the production slipforkd");
        assert_eq!(
            output.status.code(),
            Some(1),
            "a non-Btrfs state root must exit 1: {output:?}"
        );
        assert_btrfs_refusal(&stderr(&output));
        // The boot gate runs before the engine creates any snapshot, world, or control directory.
        assert!(
            !layout.state_dir.join("worlds").exists(),
            "no world directory may exist after a refused boot"
        );
        assert!(
            !layout.state_dir.join("snapshots").exists(),
            "no snapshot directory may exist after a refused boot"
        );
        assert!(
            !layout.runtime_dir.join("daemon.sock").exists(),
            "a refused daemon never binds its control socket"
        );
        eprintln!(
            "non-Btrfs host: the live Btrfs + crun + postgres:18 world boot runs on the Btrfs VM"
        );
        return;
    }

    // Btrfs host: the same spawn helper the CLI-critical suites use, running the real child.
    let repo = repo_with_manifest(scratch, "acme-live", LIVE_SNOW);
    let daemon = Daemon::start_in(scratch, &layout).expect("start the production slipforkd");
    assert!(
        !daemon.is_in_process(),
        "a Btrfs host must drive the real slipforkd child"
    );

    let up = daemon.cli(&repo, &["up", "--no-dump"]);
    assert_success(
        &up,
        "the production composition root boots a real postgres:18 world",
    );
    assert_eq!(
        stdout(&up).matches("http://").count(),
        1,
        "a booted world carries exactly one origin URL: {}",
        stdout(&up)
    );

    let snapshots = fs::read_dir(layout.state_dir.join("snapshots"))
        .expect("the sealed base snapshots directory exists")
        .filter_map(Result::ok)
        .map(|entry| entry.file_name().to_string_lossy().into_owned())
        .collect::<Vec<_>>();
    assert!(
        snapshots
            .iter()
            .any(|name| name.starts_with("acme-live-base-")),
        "the real ioctl path must seal <project>-base-<sha12>: {snapshots:?}"
    );

    let exec = daemon.cli(&repo, &["exec", "--", "true"]);
    assert_success(&exec, "exec inside the live Btrfs world");

    let list = daemon.cli(&repo, &["--json", "list"]);
    assert_success(&list, "list --json");
    let worlds: serde_json::Value = serde_json::from_str(&stdout(&list)).expect("list is JSON");
    assert_eq!(
        worlds.as_array().map(Vec::len),
        Some(1),
        "one live canonical world: {worlds}"
    );

    assert_success(&daemon.cli(&repo, &["down"]), "down the live world");
}
