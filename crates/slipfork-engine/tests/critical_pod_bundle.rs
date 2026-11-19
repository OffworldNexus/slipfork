//! Protected lab test for per-service pod OCI bundles (`ARCHITECTURE.md` §3.3,
//! `docs/design/forkfile.md` §3).
//!
//! This suite materializes a **real** `alpine:3.21` rootfs through the production
//! [`slipfork_engine::rootfs::DockerImageStore`] and writes a real bundle onto disk, so it needs
//! Docker but no Btrfs: the bundle is pure data, and the `crun` boot journey is the next slice.
//!
//! The bundle proves the three facts the pod runtime owns: `process.cwd` is the service directory,
//! `linux.namespaces[]` inherits the shared init's `ipc` namespace through the engine's `nsenter`
//! prefix while creating an identity-mapped `user` namespace plus fresh `pid`/`mount`/`uts` (never
//! a join `path`, never a `network` namespace), and **every path in the document is a
//! world-visible guest path** (`root.path`, `process.cwd`, bind sources) — never a host path that
//! only resolves because a mount is 1:1.
//! No JSON is synthesized by hand.

pub mod support;

use std::collections::BTreeMap;
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;

use slipfork_engine::current_uid_gid;
use slipfork_engine::write_pod_bundle;
use slipfork_world::{
    PodCommand, PodLocation, PodNamespaces, ServicePodSpec, VOLUMES_DIR, VolumePlan,
};

const DATABASE_URL: &str = "postgres://pod_super_secret@127.0.0.1:5432/app";

/// The real namespace links of this test process, used as the injected shared-init joins.
fn process_namespaces() -> PodNamespaces {
    let link = |name: &str| {
        let path = Path::new("/proc/self/ns").join(name);
        assert!(
            path.exists(),
            "the lab host must expose its own {} namespace link",
            path.display()
        );
        path
    };
    PodNamespaces::new(&link("user"), &link("net"), &link("ipc")).expect("absolute joins")
}

#[test]
fn a_real_alpine_pod_bundle_binds_cwd_volumes_and_shared_namespaces() {
    let image = support::oci_rootfs("alpine:3.21", &[]);
    assert!(
        image.digest.starts_with("sha256:"),
        "the materialized rootfs must carry its immutable digest: {}",
        image.digest
    );
    let release = fs::read_to_string(image.rootfs.join("etc/alpine-release"))
        .expect("a real alpine rootfs carries /etc/alpine-release");
    assert!(
        release.starts_with("3.21"),
        "the lab rootfs must be alpine 3.21, got '{release}'"
    );
    let namespaces = process_namespaces();

    let scratch = support::disk_tempdir("pod-bundle");
    let world_subvol = scratch.path().join("worlds/api-0");
    let worktree = scratch.path().join("repo");
    let service_dir = worktree.join("api");
    for dir in [&world_subvol, &service_dir] {
        fs::create_dir_all(dir).expect("scratch dir");
    }
    let bundle_dir = scratch.path().join("ctl/api-0/oci/api");
    assert!(
        !bundle_dir.starts_with(&world_subvol),
        "a pod bundle must live in the control directory, never the world subvolume"
    );

    let volumes = VolumePlan::new(&world_subvol, "api", &service_dir, &[".venv".to_owned()])
        .expect("volume plan");
    let command = PodCommand::new(
        service_dir.clone(),
        vec![
            "sh".to_owned(),
            "-lc".to_owned(),
            "exec uvicorn app:app".to_owned(),
        ],
        BTreeMap::from([(String::from("DATABASE_URL"), DATABASE_URL.to_owned())]),
    );
    let spec = ServicePodSpec::new(
        PodLocation::new(&world_subvol, &bundle_dir, &image.rootfs).expect("pod locations"),
        namespaces.clone(),
        volumes.clone(),
        command,
    )
    .expect("service pod spec");
    assert!(
        !format!("{spec:?}").contains(DATABASE_URL),
        "a pod spec must never render its resolved secret values"
    );

    let (uid, gid) = current_uid_gid();
    let config_path = write_pod_bundle(uid, gid, &worktree, &spec)
        .unwrap_or_else(|error| panic!("write the pod bundle:\n{error}"));
    assert_eq!(config_path, bundle_dir.join("config.json"));
    let mode = |path: &Path| fs::metadata(path).expect("metadata").permissions().mode() & 0o777;
    assert_eq!(mode(&bundle_dir), 0o700, "the bundle carries secrets");
    assert_eq!(mode(&config_path), 0o600, "config.json carries secrets");

    let raw = fs::read_to_string(&config_path).expect("read config.json");
    let config: serde_json::Value = serde_json::from_str(&raw).expect("parse config.json");
    assert_eq!(config["ociVersion"], "1.0.2");
    assert_eq!(
        config["root"]["path"],
        image.rootfs.display().to_string(),
        "root.path must be the world-visible guest rootfs, never the host cache path"
    );
    assert_eq!(config["root"]["readonly"], true);
    assert_eq!(
        config["process"]["cwd"],
        service_dir.display().to_string(),
        "process.cwd is the service directory from the spec (mapped 1:1 by the worktree mount)"
    );
    assert_eq!(
        config["process"]["args"],
        serde_json::json!(["sh", "-lc", "exec uvicorn app:app"])
    );
    let env: Vec<&str> = config["process"]["env"]
        .as_array()
        .expect("env array")
        .iter()
        .map(|value| value.as_str().expect("env string"))
        .collect();
    let expected_env = format!("DATABASE_URL={DATABASE_URL}");
    assert!(
        env.contains(&expected_env.as_str()),
        "the bundle carries the spec's complete environment: {env:?}"
    );

    // The pod inherits the world's IPC namespace through the engine's `nsenter` prefix (an OCI
    // `path` join cannot be opened from inside the world) and creates its own identity-mapped
    // user namespace plus a fresh pid/mount/uts set. It never names a join path and never creates
    // a network namespace: it shares the world's `127.0.0.1`.
    let namespaces_json = config["linux"]["namespaces"]
        .as_array()
        .expect("namespaces");
    let kinds: Vec<&str> = namespaces_json
        .iter()
        .map(|entry| entry["type"].as_str().expect("namespace type"))
        .collect();
    assert_eq!(
        kinds,
        ["user", "pid", "mount", "uts"],
        "a pod creates exactly these namespaces and inherits `ipc`: {namespaces_json:?}"
    );
    assert!(
        namespaces_json.iter().all(|entry| entry["path"].is_null()),
        "no namespace may be joined by path from inside the world: {namespaces_json:?}"
    );
    assert!(
        !kinds.contains(&"network"),
        "a pod shares the world's `127.0.0.1` and creates no network namespace: {namespaces_json:?}"
    );
    assert!(
        spec.namespace_joins()
            .iter()
            .any(|(kind, _path)| *kind == "ipc"),
        "the pod spec still supplies the shared-init IPC join"
    );
    assert_eq!(
        config["linux"]["uidMappings"],
        serde_json::json!([{ "containerID": uid, "hostID": uid, "size": 1 }]),
        "the pod creates an identity-mapped user namespace"
    );
    assert_eq!(
        config["linux"]["gidMappings"],
        serde_json::json!([{ "containerID": gid, "hostID": gid, "size": 1 }]),
        "the pod creates an identity-mapped group namespace"
    );

    let binds: Vec<(String, String, Vec<String>)> = config["mounts"]
        .as_array()
        .expect("mounts")
        .iter()
        .filter(|mount| mount["type"] == "bind")
        .map(|mount| {
            (
                mount["source"].as_str().expect("source").to_owned(),
                mount["destination"]
                    .as_str()
                    .expect("destination")
                    .to_owned(),
                mount["options"]
                    .as_array()
                    .expect("options")
                    .iter()
                    .map(|option| option.as_str().expect("option").to_owned())
                    .collect(),
            )
        })
        .collect();
    let worktree_path = worktree.display().to_string();
    let live = binds
        .iter()
        .find(|(_, destination, _)| destination == &worktree_path)
        .unwrap_or_else(|| panic!("no live-worktree bind in {binds:?}"));
    assert_eq!(
        live,
        &(
            worktree_path.clone(),
            worktree_path.clone(),
            vec!["rw".to_owned(), "rbind".to_owned()],
        ),
        "the live worktree is bound read-write"
    );
    let volume_target = service_dir.join(".venv").display().to_string();
    let volume = binds
        .iter()
        .find(|(_, destination, _)| destination == &volume_target)
        .unwrap_or_else(|| panic!("no CoW volume bind in {binds:?}"));
    assert_eq!(
        volume,
        &(
            world_subvol
                .join(VOLUMES_DIR)
                .join("api/.venv")
                .display()
                .to_string(),
            volume_target.clone(),
            vec!["rw".to_owned(), "rbind".to_owned()],
        ),
        "each CoW volume binds from its guest-path subvolume, never a host path"
    );
    let live_order = binds
        .iter()
        .position(|(_, destination, _)| destination == &worktree_path)
        .expect("live worktree bind");
    let volume_order = binds
        .iter()
        .position(|(_, destination, _)| destination == &volume_target)
        .expect("volume bind");
    assert!(
        live_order < volume_order,
        "the live worktree must bind before its volumes so each volume overrides its subtree"
    );
}
