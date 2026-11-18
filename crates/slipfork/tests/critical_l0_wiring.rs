//! Protected `L0` wiring tests (`M2.29`, `ADR-0009` §1–§3).
//!
//! These tests boot real `BtrfsEngine` worlds through the harness `slipforkd` composition root
//! (the real child, or the same in-process daemon graph over the real kernel driver), extract the
//! real pinned `crun` + `redis:8` OCI image, and assert the container-scoped CLI contract: `db`/`redis`
//! resolve only through `WorldEngine::service_entry_command`, a stopped container fails closed with
//! `ServiceNotRunning`, and `--host-caches` refuses on the measured ELF/libc ABI mismatch while
//! passing on a matching glibc world. No test skips: a missing Docker/crun dependency fails loudly.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::Duration;

use slipfork_edge::daemon::ExecTargetRequest;
use slipfork_edge::{DaemonControl, DaemonControlClient, DaemonErrorKind, EdgeError};

pub mod support;
use support::{Daemon, assert_success, repo_with_manifest, stderr, stdout};

/// Bound on the daemon noticing a stopped container: `redis-cli shutdown` stops the container's
/// PID 1, and the daemon must then answer the next entry with the typed `ServiceNotRunning`.
const REDIS_STOP_DEADLINE: Duration = Duration::from_secs(20);

const ALPINE_SNOW: &str = r#"
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
  redis_clusters:
    - id: cache
      version: "8"

modes:
  local:
    envs:
      app:
        BASE_URL: '${{ resources::app.url }}'
"#;

const DEBIAN_DOCKERFILE: &str = "FROM debian:trixie-slim AS app\nWORKDIR /workspace\n";

const DEBIAN_SNOW: &str = r#"
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

modes:
  local:
    envs:
      app:
        BASE_URL: '${{ resources::app.url }}'
"#;

fn alpine_repo(scratch: &Path) -> PathBuf {
    repo_with_manifest(scratch, "acme-l0", ALPINE_SNOW)
}

fn glibc_repo(scratch: &Path) -> PathBuf {
    let repo = repo_with_manifest(scratch, "acme-glibc", DEBIAN_SNOW);
    std::fs::write(repo.join("Dockerfile"), DEBIAN_DOCKERFILE).expect("write glibc Dockerfile");
    support::run_git(&repo, &["add", "Dockerfile"]);
    support::run_git(&repo, &["commit", "-q", "-m", "glibc fixture"]);
    repo
}

/// Drive the entry command the daemon returned, exactly like the thin CLI does.
fn run_entry(target: &slipfork_edge::WorldExecTarget, argv: &[&str]) -> std::process::ExitStatus {
    let mut args = target.entry.args_prefix.clone();
    // `crun exec` takes the command directly after the container id; its own CLI does not strip a
    // trailing `--` separator (recorded as an engine follow-up in RESULT.md).
    if args.last().map(String::as_str) == Some("--") {
        args.pop();
    }
    let mut command = Command::new(&target.entry.program);
    command
        .args(&args)
        .args(argv)
        .current_dir(&target.entry.cwd)
        .env_clear()
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::inherit());
    for (name, value) in target.env.iter() {
        command.env(name, value.as_ref());
    }
    command.status().expect("spawn service entry")
}

#[test]
fn container_backed_world_boots_and_redis_targets_the_container() {
    let tmp = support::disk_tempdir("l0-");
    let scratch = tmp.path();
    let repo = alpine_repo(scratch);
    let daemon = Daemon::start(scratch);

    let up = daemon.cli(&repo, &["up"]);
    assert_success(&up, "up with a real redis:8 backing-service container");

    let client = DaemonControlClient::bind(&daemon.runtime_dir);
    let target = client
        .exec_target(&ExecTargetRequest {
            cwd: repo.clone(),
            world: None,
            service: Some("redis_clusters.cache".to_owned()),
            argv: vec!["redis-cli".to_owned()],
        })
        .expect("a running redis container is enterable through the typed control port");
    assert_eq!(target.entry.program, PathBuf::from("nsenter"));
    assert!(
        target
            .entry
            .args_prefix
            .iter()
            .any(|arg| arg == "--cgroup-manager=disabled"),
        "the entry must be the daemonless crun invocation: {:?}",
        target.entry.args_prefix
    );
    assert!(
        target.entry.args_prefix.iter().any(|arg| arg == "exec"),
        "the entry must be `crun exec`, never the app rootfs: {:?}",
        target.entry.args_prefix
    );
    assert_eq!(target.target_argv, vec!["redis-cli".to_owned()]);
    assert!(
        target.env.iter().all(|(name, _)| name != "PATH"),
        "the container's own OCI environment must not be replaced by the app rootfs PATH"
    );
}

#[test]
fn redis_entry_fails_closed_with_service_not_running_after_the_container_stops() {
    let tmp = support::disk_tempdir("l0-");
    let scratch = tmp.path();
    let repo = alpine_repo(scratch);
    let daemon = Daemon::start(scratch);
    assert_success(&daemon.cli(&repo, &["up"]), "up with redis");

    let client = DaemonControlClient::bind(&daemon.runtime_dir);
    let request = ExecTargetRequest {
        cwd: repo.clone(),
        world: None,
        service: Some("redis_clusters.cache".to_owned()),
        argv: vec!["redis-cli".to_owned()],
    };
    let target = client.exec_target(&request).expect("running redis entry");
    // `redis-cli shutdown` stops the container's PID 1, so the `crun exec` process itself is
    // reaped with it; the observable contract is the typed `ServiceNotRunning` below.
    let _status = run_entry(&target, &["redis-cli", "shutdown", "nosave"]);

    let stopped = support::poll_until(REDIS_STOP_DEADLINE, support::POLL_INTERVAL, || match client
        .exec_target(&request)
    {
        Err(EdgeError::Control(error)) if error.kind == DaemonErrorKind::ServiceNotRunning => {
            assert!(
                error.message.contains("redis_clusters.cache")
                    && error.message.contains("ServiceNotRunning"),
                "the refusal must name the stopped service: {}",
                error.message
            );
            assert!(
                !error.message.contains("rootfs"),
                "a stopped container never falls back to the app rootfs: {}",
                error.message
            );
            Ok(())
        }
        Err(other) => Err(format!(
            "expected ServiceNotRunning after shutdown, got: {other}"
        )),
        Ok(_) => Err("the stopped redis container must never be re-entered".to_owned()),
    });
    if let Err(last) = stopped {
        panic!("{last}");
    }

    let cli = daemon.cli(&repo, &["redis"]);
    assert!(
        !cli.status.success(),
        "`slipfork redis` must fail closed after the container stops"
    );
    let diagnostic = stderr(&cli);
    assert!(
        diagnostic.contains("ServiceNotRunning") && diagnostic.contains("redis_clusters.cache"),
        "the CLI must surface the typed ServiceNotRunning refusal: {diagnostic}"
    );
    assert!(
        stdout(&cli).trim().is_empty(),
        "a refused service entry produces no output"
    );
}

#[test]
fn host_caches_refuses_on_the_measured_abi_mismatch() {
    let tmp = support::disk_tempdir("l0-");
    let scratch = tmp.path();
    let repo = alpine_repo(scratch);
    let daemon = Daemon::start(scratch);

    let up = daemon.cli(&repo, &["up", "--host-caches"]);
    assert!(
        !up.status.success(),
        "an Alpine (musl) world must refuse glibc host caches"
    );
    let diagnostic = stderr(&up);
    for needle in [
        "`--host-caches` ABI check failed",
        "host is x86_64 glibc",
        "world stage 'app' is x86_64 musl",
    ] {
        assert!(
            diagnostic.contains(needle),
            "the measured ABI mismatch must be reported verbatim ({needle}): {diagnostic}"
        );
    }
}

#[test]
fn host_caches_pass_on_a_matching_glibc_world() {
    let tmp = support::disk_tempdir("l0-");
    let scratch = tmp.path();
    let repo = glibc_repo(scratch);
    let daemon = Daemon::start(scratch);

    let up = daemon.cli(&repo, &["up", "--host-caches"]);
    assert_success(&up, "up --host-caches on a matching glibc world");
}
