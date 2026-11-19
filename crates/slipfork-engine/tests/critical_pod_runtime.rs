//! Protected lab journey: one real service pod boots as a `crun` container joined to the world's
//! shared `user`/`network`/`ipc` namespaces, then answers `crun exec` inside its own bundle
//! (`ARCHITECTURE.md` §3.3).
//!
//! This is a Btrfs world-booting journey: it builds a real `alpine:3.21`-derived rootfs, clones a
//! real Btrfs world, writes a real OCI bundle and runs the pinned static `crun`. On a non-Btrfs
//! host it fails closed through [`support::WorldFixture`]'s `require_btrfs` gate (one line naming
//! the missing substrate) — never a skip.

pub mod support;

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::Duration;

use slipfork_world::{
    EngineError, ExecEnv, PodCommand, PodLocation, PodNamespaces, ServiceKind, ServicePodSpec,
    ServiceRuntime, ServiceSpec, ServiceState, VolumePlan, WorldEngine, WorldTarget,
};
use support::WorldFixture;

/// Port the pod's readiness contract waits for inside the joined world network namespace.
const POD_PORT: u16 = 8081;

/// The bundle creates its own user/pid/mount/uts namespaces and inherits the world's IPC and
/// network namespaces from the engine's `nsenter` prefix (an OCI `path` join is unreadable inside
/// the world).
fn assert_fresh_namespaces(config: &serde_json::Value) {
    let namespaces = config["linux"]["namespaces"]
        .as_array()
        .expect("namespaces");
    let kinds: Vec<&str> = namespaces
        .iter()
        .map(|entry| entry["type"].as_str().expect("type"))
        .collect();
    assert_eq!(
        kinds,
        ["user", "pid", "mount", "uts"],
        "IPC/network are inherited from the world, never recreated: {namespaces:?}"
    );
    assert!(
        namespaces.iter().all(|entry| entry["path"].is_null()),
        "no OCI path join may reach the bundle: {namespaces:?}"
    );
    assert!(
        config["linux"]["uidMappings"].is_array(),
        "the identity-mapped user namespace must declare its map"
    );
}

/// Build one pod spec for `service`, optionally overriding its bundle/cwd/argv.
fn pod_spec(
    fx: &WorldFixture,
    namespaces: &PodNamespaces,
    service: &str,
    bundle: PathBuf,
    cwd: PathBuf,
    argv: Vec<String>,
) -> ServicePodSpec {
    ServicePodSpec::new(
        PodLocation::new(&fx.state_dir.join("worlds"), &bundle, &fx.rootfs.app_rootfs)
            .expect("absolute pod locations"),
        namespaces.clone(),
        VolumePlan::new(&fx.state_dir.join("worlds"), service, &cwd, &[])
            .expect("valid volume plan"),
        PodCommand::new(
            cwd,
            argv,
            BTreeMap::from([
                ("PATH".to_owned(), "/usr/local/bin:/usr/bin:/bin".to_owned()),
                ("POD_TOKEN".to_owned(), "pod-secret-value".to_owned()),
            ]),
        ),
    )
    .expect("service pod spec")
}

#[test]
fn a_service_pod_boots_into_the_world_namespaces_and_runs_crun_exec() {
    let fx = WorldFixture::new("service-pod");
    let world = fx.world(WorldTarget::Canonical);

    let namespaces = fx
        .engine
        .world_pod_namespaces(&world)
        .expect("a booted world publishes its shared-init joins");
    let bundle = fx
        .engine
        .pod_bundle_dir(&world, "app")
        .expect("the engine owns the pod bundle directory");
    assert!(
        !bundle.starts_with(fx.state_dir.join("worlds")),
        "a pod bundle lives in the control directory, never the world subvolume"
    );

    let service_dir = fx.repo.join("app");
    fs::create_dir_all(&service_dir).expect("service directory");
    // The pod prints a readiness marker and then listens on the world's loopback: spawn must only
    // return once the joined namespace accepts connections on `POD_PORT`.
    let script = format!(
        "printf 'pod-ready\\n'; exec python3 -c 'import socket,time; s=socket.socket(); \
         s.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1); \
         s.bind((\"127.0.0.1\", {POD_PORT})); s.listen(); time.sleep(600)'"
    );
    let pod = pod_spec(
        &fx,
        &namespaces,
        "app",
        bundle.clone(),
        service_dir.clone(),
        vec!["sh".to_owned(), "-c".to_owned(), script],
    );
    let spec = ServiceSpec {
        id: "app".to_owned(),
        kind: ServiceKind::Component,
        runtime: ServiceRuntime::ServicePod(pod),
        env: ExecEnv::default(),
        readiness_port: Some(POD_PORT),
    };
    let handle = fx
        .engine
        .spawn_service(&world, &spec)
        .expect("spawn the service pod and wait for its readiness port");

    // The bundle the engine wrote is the container's real declaration: cwd, namespace joins, mode.
    // It lives in the world's private tmpfs, so the host mountpoint stays empty and only an
    // in-world `exec` can read it.
    let config_path = bundle.join("config.json");
    assert!(
        !config_path.exists(),
        "the bundle config must never appear on the host state directory: {}",
        config_path.display()
    );
    let config_text = fx.world_sh(&world, &format!("cat '{}'", config_path.display()));
    assert_eq!(
        config_text.exit_code, 0,
        "read the in-world bundle config: {}",
        config_text.stderr
    );
    let config: serde_json::Value = serde_json::from_str(&config_text.stdout).expect("bundle json");
    assert_eq!(
        fx.world_sh(&world, &format!("stat -c %a '{}'", config_path.display()))
            .stdout
            .trim(),
        "600",
        "the bundle carries resolved secrets"
    );
    assert_eq!(
        config["process"]["cwd"],
        service_dir.display().to_string(),
        "process.cwd must be the service directory"
    );
    assert_fresh_namespaces(&config);
    assert_eq!(
        config["process"]["env"],
        serde_json::json!([
            "PATH=/usr/local/bin:/usr/bin:/bin",
            "POD_TOKEN=pod-secret-value"
        ]),
        "the bundle carries exactly the pod's declared environment"
    );

    // The retained log ring really receives the container's stdout.
    let mut logged = String::new();
    support::poll_until(Duration::from_secs(30), support::POLL_INTERVAL, || {
        logged = fx
            .engine
            .service_logs(&world, "app")
            .map(|logs| {
                logs.snapshot()
                    .iter()
                    .map(|chunk| String::from_utf8_lossy(&chunk.bytes).into_owned())
                    .collect::<String>()
            })
            .unwrap_or_default();
        if logged.contains("pod-ready") {
            Ok(())
        } else {
            Err("the pod has not logged `pod-ready` yet".to_owned())
        }
    })
    .expect("the pod's stdout must reach the retained log ring");
    assert!(logged.contains("pod-ready"), "{logged}");

    let row = fx
        .engine
        .world_processes(&world)
        .expect("service rows")
        .into_iter()
        .find(|row| row.id == "app")
        .expect("pod row");
    assert_eq!(row.state, ServiceState::Running);
    assert_eq!(row.port, Some(POD_PORT), "the pod reports its serving port");
    let supervisor_pid = row.pid.expect("a running pod reports its supervisor pid");
    assert!(
        Path::new(&format!("/proc/{supervisor_pid}")).exists(),
        "the pod supervisor is alive"
    );

    // `crun exec` runs inside the running container: its declared env and the world netns. Every
    // container inherits the world init's network namespace, so a one-shot world exec and the pod
    // must report the same namespace identity (read with `-L`: the `/proc` link is a magic symlink).
    let world_netns = fx
        .exec(&world, "stat -Lc %i /proc/1/ns/net")
        .stdout
        .trim()
        .to_owned();
    assert!(!world_netns.is_empty(), "the world init netns exists");
    let exec = fx
        .engine
        .exec_in_service(
            &handle,
            &[
                "sh".to_owned(),
                "-c".to_owned(),
                "printf '%s|%s' \"$POD_TOKEN\" \"$(stat -Lc %i /proc/1/ns/net)\"".to_owned(),
            ],
            &ExecEnv::default(),
            &mut |_chunk| {},
        )
        .expect("exec inside the service pod");
    assert_eq!(exec.exit_code, 0, "crun exec stderr: {}", exec.stderr);
    assert_eq!(
        exec.stdout,
        format!("pod-secret-value|{world_netns}"),
        "the pod's declared env and the world's network namespace apply"
    );
    // The pod must join the world's IPC namespace too: compare this process's own view on both
    // surfaces (the pod's bundle creates a fresh PID namespace, so `/proc/1` is not the world init).
    let world_ipc = fx
        .exec(&world, "stat -Lc %i /proc/self/ns/ipc")
        .stdout
        .trim()
        .to_owned();
    assert!(!world_ipc.is_empty(), "the world exec IPC namespace exists");
    let ipc = fx
        .engine
        .exec_in_service(
            &handle,
            &[
                "sh".to_owned(),
                "-c".to_owned(),
                "stat -Lc %i /proc/self/ns/ipc".to_owned(),
            ],
            &ExecEnv::default(),
            &mut |_chunk| {},
        )
        .expect("exec the IPC namespace probe inside the service pod");
    assert_eq!(ipc.exit_code, 0, "crun exec stderr: {}", ipc.stderr);
    assert_eq!(
        ipc.stdout.trim(),
        world_ipc,
        "the pod must join the world's IPC namespace"
    );

    let entry = fx
        .engine
        .service_entry_command(&handle)
        .expect("entry command");
    assert!(
        entry.args_prefix.contains(&"exec".to_owned()),
        "entry enters through `crun exec`: {:?}",
        entry.args_prefix
    );
    assert!(
        matches!(
            fx.engine.update_service_env(&handle, &ExecEnv::default()),
            Err(EngineError::ContainerEnvFixed { .. })
        ),
        "a pod's bundle env is fixed at spawn"
    );

    fx.engine.stop_service(&handle).expect("stop the pod");
    assert!(
        !Path::new(&format!("/proc/{supervisor_pid}")).exists(),
        "stop reaps the pod supervisor (pid {supervisor_pid} must leave /proc)"
    );
    let row = fx
        .engine
        .world_processes(&world)
        .expect("retained rows")
        .into_iter()
        .find(|row| row.id == "app")
        .expect("a stopped pod keeps its row");
    assert_eq!(row.state, ServiceState::Stopped);
    assert_eq!(row.pid, None);
    assert!(
        !config_path.exists(),
        "stop removes the pod bundle from the world's tmpfs"
    );

    // Fail-closed refusals on the same boot: a bundle outside the engine's directory and a cwd
    // outside every read-write world mount both stop before a bundle or a running row exists.
    for (id, bundle_dir, cwd) in [
        (
            "foreign",
            fx.root.join("foreign-bundle/app"),
            service_dir.clone(),
        ),
        (
            "outside",
            fx.engine
                .pod_bundle_dir(&world, "outside")
                .expect("engine bundle dir"),
            PathBuf::from("/var/empty/outside-pod"),
        ),
    ] {
        let pod = pod_spec(
            &fx,
            &namespaces,
            id,
            bundle_dir.clone(),
            cwd,
            vec!["sleep".to_owned(), "600".to_owned()],
        );
        let err = fx
            .engine
            .spawn_service(
                &world,
                &ServiceSpec {
                    id: id.to_owned(),
                    kind: ServiceKind::Component,
                    runtime: ServiceRuntime::ServicePod(pod),
                    env: ExecEnv::default(),
                    readiness_port: None,
                },
            )
            .expect_err("a pod the engine cannot place must fail closed");
        assert!(
            matches!(err, EngineError::InvalidWorldSpec { .. }),
            "expected InvalidWorldSpec for '{id}', got {err:?}"
        );
        assert!(
            !bundle_dir.exists(),
            "'{id}' must not leave a bundle behind"
        );
        let rows = fx.engine.world_processes(&world).expect("rows");
        if let Some(row) = rows.iter().find(|row| row.id == id) {
            assert_ne!(
                row.state,
                ServiceState::Running,
                "'{id}' was refused but is reported running"
            );
            assert_eq!(row.pid, None);
        }
    }

    fx.engine.destroy_world(world).expect("destroy the world");
}
