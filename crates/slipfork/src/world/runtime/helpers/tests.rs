use std::fs;

use tempfile::TempDir;

use super::{ServiceLaunch, project_binds};
use slipfork_world::PodEnv;

/// `/project` is a dev-only bind: dropping `dev` leaves pods on the built rootfs alone.
#[test]
fn only_a_dev_world_binds_the_live_project() {
    let source = "# forkfile: 0.1\n\nimage base from python:3.13-slim {\n  service api {\n    run: sleep 3600\n  }\n}\n";
    let checked = slipfork_forkfile::compile(source).expect("compile the fixture");
    let dev = checked.document.default_features();
    assert!(dev.contains("dev"));
    let binds = project_binds(&dev, std::path::Path::new("/var/tmp/demo"));
    assert_eq!(binds.len(), 1);
    assert_eq!(binds[0].destination, "/project");

    let bare = checked
        .document
        .resolve_features(true, &[])
        .expect("resolve an empty base");
    assert!(!bare.contains("dev"));
    assert!(
        project_binds(&bare, std::path::Path::new("/var/tmp/demo")).is_empty(),
        "a non-dev world must not see the live checkout"
    );
}

/// A `WORKDIR /app` + `COPY . /srv` stage overlays the context at both destinations; a
/// `COPY . .` stage keeps one `/app` overlay.
#[test]
fn a_dev_mount_binds_the_context_at_the_workdir_and_every_copy_destination() {
    let checked = slipfork_forkfile::compile("# forkfile: 0.1\n\nfeatures {\n  dev\n}\n")
        .expect("compile the fixture");
    let dev = checked.document.default_features();
    let project = std::path::Path::new("/var/tmp/demo");
    let split = super::DevMount {
        workdir: "/app".to_owned(),
        context: std::path::PathBuf::from("/var/tmp/demo"),
        overlays: vec!["/app".to_owned(), "/srv".to_owned()],
    };
    let binds = super::dev_binds(&dev, project, Some(&split));
    let destinations: Vec<&str> = binds.iter().map(|bind| bind.destination.as_str()).collect();
    assert_eq!(destinations, ["/project", "/app", "/srv"]);
    assert!(
        binds.iter().all(|bind| bind.destination != "/"),
        "a dev bind must never overlay the rootfs root"
    );

    let same = super::DevMount {
        overlays: vec!["/app".to_owned()],
        ..split
    };
    let same_binds = super::dev_binds(&dev, project, Some(&same));
    let destinations: Vec<&str> = same_binds
        .iter()
        .map(|bind| bind.destination.as_str())
        .collect();
    assert_eq!(destinations, ["/project", "/app"]);
}

fn touch(root: &TempDir, relative: &str) {
    let path = root.path().join(relative);
    fs::create_dir_all(path.parent().expect("fixture parent")).expect("create fixture dirs");
    fs::write(path, b"#!/bin/sh\n").expect("write fixture binary");
}

fn path_of(launch: &ServiceLaunch) -> String {
    launch
        .env
        .iter()
        .find(|entry| entry.key == "PATH")
        .map(|entry| entry.value.clone())
        .expect("resolved launch carries PATH")
}

#[test]
fn merge_path_prefers_a_declared_forkfile_path_then_the_image_path() {
    let declared = vec![PodEnv::new("PATH", "/declared/bin")];
    let image = vec!["PATH=/image/bin".to_owned()];
    assert_eq!(super::merge_path(&declared, &image), "/declared/bin");
    assert_eq!(super::merge_path(&[], &image), "/image/bin");
    assert_eq!(super::merge_path(&[], &[]), super::DEFAULT_PATH);

    let mut env = vec![
        PodEnv::new("PATH", "/declared/bin"),
        PodEnv::new("HOME", "/root"),
    ];
    super::upsert_env(&mut env, vec![PodEnv::new("PATH", "/shim:/declared/bin")]);
    assert_eq!(env.iter().filter(|entry| entry.key == "PATH").count(), 1);
    assert_eq!(env[1].value, "/shim:/declared/bin");
}

#[test]
fn uv_run_resolves_to_the_project_venv_binary_without_the_wrapper() {
    let project = TempDir::new().expect("project");
    let rootfs = TempDir::new().expect("rootfs");
    touch(&project, ".venv/bin/uvicorn");
    let launch = ServiceLaunch::resolve(
        project.path(),
        "/project",
        rootfs.path(),
        super::DEFAULT_PATH,
        "@uv run uvicorn app.main:app --reload --host 0.0.0.0 --port 8000",
    );
    assert_eq!(
        launch.argv,
        [
            "/project/.venv/bin/uvicorn",
            "app.main:app",
            "--reload",
            "--host",
            "0.0.0.0",
            "--port",
            "8000"
        ]
    );
    assert!(path_of(&launch).starts_with("/project/.venv/bin:"));
    assert!(path_of(&launch).ends_with(super::DEFAULT_PATH));
    assert!(
        launch
            .env
            .iter()
            .any(|entry| entry.key == "VIRTUAL_ENV" && entry.value == "/project/.venv")
    );
}

#[test]
fn pnpm_exec_resolves_to_the_workspace_shim_in_a_subdirectory() {
    let project = TempDir::new().expect("project");
    let rootfs = TempDir::new().expect("rootfs");
    touch(&project, "front/node_modules/.bin/vite");
    let launch = ServiceLaunch::resolve(
        project.path(),
        "/project/front",
        rootfs.path(),
        "/image/bin",
        "@pnpm exec vite dev --host 0.0.0.0 --port 5173",
    );
    assert_eq!(
        launch.argv,
        [
            "/project/front/node_modules/.bin/vite",
            "dev",
            "--host",
            "0.0.0.0",
            "--port",
            "5173"
        ]
    );
    assert_eq!(
        path_of(&launch),
        "/project/front/node_modules/.bin:/image/bin"
    );
    assert!(
        !launch.env.iter().any(|entry| entry.key == "VIRTUAL_ENV"),
        "a node wrapper must not export a Python venv"
    );
}

#[test]
fn a_missing_shim_keeps_the_original_wrapper() {
    let project = TempDir::new().expect("project");
    let rootfs = TempDir::new().expect("rootfs");
    let launch = ServiceLaunch::resolve(
        project.path(),
        "/project",
        rootfs.path(),
        super::DEFAULT_PATH,
        "@uv run python -m app.worker",
    );
    assert_eq!(
        launch.argv,
        ["/bin/sh", "-c", "uv run python -m app.worker"]
    );
    assert!(launch.env.is_empty());
}

#[test]
fn a_flag_first_command_and_a_foreign_workdir_both_fall_back() {
    let project = TempDir::new().expect("project");
    let rootfs = TempDir::new().expect("rootfs");
    touch(&project, ".venv/bin/uvicorn");
    let flag_first = ServiceLaunch::resolve(
        project.path(),
        "/project",
        rootfs.path(),
        super::DEFAULT_PATH,
        "@uv run -m uvicorn",
    );
    assert_eq!(flag_first.argv, ["/bin/sh", "-c", "uv run -m uvicorn"]);
    let foreign = ServiceLaunch::resolve(
        project.path(),
        "/opt/app",
        rootfs.path(),
        super::DEFAULT_PATH,
        "@uv run uvicorn app.main:app",
    );
    assert_eq!(
        foreign.argv,
        ["/bin/sh", "-c", "uv run uvicorn app.main:app"]
    );
}

/// One sealed head in a real snapshot store: the head record plus the image node that wraps it.
fn sealed_head(
    disk: &TempDir,
    seed: u8,
    user: Option<&str>,
    state: &str,
) -> (slipfork_world::StoreLayout, super::ImageHead) {
    sealed_head_with(disk, seed, user, state, true)
}

/// A pulled head: the `OCI` image is sealed but no node record is ever written for the head.
fn sealed_head_without_record(
    disk: &TempDir,
    seed: u8,
    state: &str,
) -> (slipfork_world::StoreLayout, super::ImageHead) {
    sealed_head_with(disk, seed, None, state, false)
}

fn sealed_head_with(
    disk: &TempDir,
    seed: u8,
    user: Option<&str>,
    state: &str,
    write_record: bool,
) -> (slipfork_world::StoreLayout, super::ImageHead) {
    use slipfork_world::{NodeKey, StoreLayout};
    let layout = StoreLayout::new(disk.path());
    let snapshots = layout.snapshots_root();
    let head = NodeKey::from_digest([seed; 32]);
    if write_record {
        fs::create_dir_all(&snapshots).expect("create the snapshots root");
        fs::write(
                snapshots.join(format!("{}.json", head.as_hex())),
                r#"{"v":1,"kind":"step","parents":[],"canon":"WORKDIR /app","line":2,"created_ms":0,"last_used_ms":0,"facts":null}"#,
            )
            .expect("write the head record");
    }
    if let Some(user) = user {
        fs::create_dir_all(&snapshots).expect("create the snapshots root");
        fs::write(
                snapshots.join(format!("{}.json", "a".repeat(64))),
                format!(
                    r#"{{"v":1,"kind":"image","parents":["{}"],"canon":"FROM scratch","line":1,"created_ms":0,"last_used_ms":0,"facts":{{"user":{}}}}}"#,
                    head.as_hex(),
                    serde_json::to_string(user).expect("encode the USER fact"),
                ),
            )
            .expect("write the image record");
    }
    let rootfs = disk.path().join("rootfs");
    fs::create_dir_all(rootfs.join("etc")).expect("seed the image etc");
    fs::write(
        rootfs.join("etc/passwd"),
        "root:x:0:0:root:/root:/bin/sh\napp:x:1000:1000:App:/home/app:/bin/sh\n",
    )
    .expect("write the image passwd");
    let head = super::ImageHead {
        key: head,
        rootfs,
        inherited_env: Vec::new(),
        argv: vec!["/bin/true".to_owned()],
        workdir: "/app".to_owned(),
        state: state.to_owned(),
    };
    (layout, head)
}

/// A from-only head's rootfs lives under `images/oci/`; a same-digest Dockerfile build's base
/// key can carry a `USER` record, and the pull must never alias it.
#[test]
fn a_pulled_head_never_aliases_another_images_user_record() {
    use slipfork_world::{BtrfsEngine, NodeKey, StoreLayout};
    let disk = TempDir::new_in("/var/tmp").expect("real disk store");
    let layout = StoreLayout::new(disk.path());
    let engine = BtrfsEngine::new(StoreLayout::new(disk.path()));
    let head_key = NodeKey::from_digest([9; 32]);
    let snapshots = layout.snapshots_root();
    fs::create_dir_all(&snapshots).expect("create the snapshots root");
    fs::write(
            snapshots.join(format!("{}.json", head_key.as_hex())),
            r#"{"v":1,"kind":"base","parents":[],"canon":"FROM sha256:c64c","line":1,"created_ms":0,"last_used_ms":0,"facts":null}"#,
        )
        .expect("write the shared base record");
    fs::write(
            snapshots.join(format!("{}.json", "b".repeat(64))),
            format!(
                r#"{{"v":1,"kind":"image","parents":["{}"],"canon":"FROM alpine:3.20","line":1,"created_ms":0,"last_used_ms":0,"facts":{{"user":"ghost"}}}}"#,
                head_key.as_hex()
            ),
        )
        .expect("write the other image's USER record");

    let rootfs = layout.images_root().join("oci").join("c".repeat(64));
    fs::create_dir_all(rootfs.join("etc")).expect("create the sealed OCI rootfs");
    let head = super::ImageHead {
        key: head_key,
        rootfs,
        inherited_env: Vec::new(),
        argv: vec!["/bin/true".to_owned()],
        workdir: "/".to_owned(),
        state: "cached".to_owned(),
    };
    assert_eq!(
        super::sealed_identity(&layout, &engine, &head)
            .expect("a pulled image is root, never the aliased USER"),
        slipfork_world::Identity::ROOT
    );
}

/// A sealed image's `USER` resolves through the same `Identity::resolve` the build uses.
#[test]
fn a_sealed_image_user_resolves_and_an_unresolvable_one_fails_closed() {
    use slipfork_world::BtrfsEngine;
    let disk = TempDir::new_in("/var/tmp").expect("real disk store");
    let engine = BtrfsEngine::new(slipfork_world::StoreLayout::new(disk.path()));

    let (layout, head) = sealed_head(&disk, 1, Some("1000:1000"), "built");
    assert_eq!(
        super::sealed_identity(&layout, &engine, &head).expect("a numeric USER resolves"),
        slipfork_world::Identity {
            uid: 1000,
            gid: 1000
        }
    );

    let (layout, head) = sealed_head(&disk, 2, Some("ghost"), "built");
    let error = super::sealed_identity(&layout, &engine, &head)
        .expect_err("an unresolvable USER must fail closed");
    assert!(
        error.cause().contains("`ghost`") && error.cause().contains("/etc/passwd"),
        "the cause names the account and its file: {}",
        error.cause()
    );
    assert!(
        error.remedy().contains("useradd"),
        "the remedy names the fix: {}",
        error.remedy()
    );
    assert!(!error.cause().contains("next:"), "no embedded remedy");
}

/// A recorded head that lost its image record fails closed; a record-less pulled head is root.
#[test]
fn a_built_head_without_its_image_record_is_refused_but_a_pulled_image_is_root() {
    use slipfork_world::BtrfsEngine;
    let disk = TempDir::new_in("/var/tmp").expect("real disk store");
    let engine = BtrfsEngine::new(slipfork_world::StoreLayout::new(disk.path()));

    let (layout, head) = sealed_head(&disk, 3, None, "built");
    let error = super::sealed_identity(&layout, &engine, &head).expect_err("fail closed");
    assert!(
        error.cause().contains("no recorded `USER`"),
        "{}",
        error.cause()
    );
    assert!(error.remedy().contains("slipfork up"), "{}", error.remedy());

    let (layout, head) = sealed_head(&disk, 4, None, "pulled");
    assert_eq!(
        super::sealed_identity(&layout, &engine, &head).expect("a pulled image is 0:0"),
        slipfork_world::Identity::ROOT
    );

    // The second `up` reports the from-only image as `cached` and still writes no node
    // record, so its absent `USER` stays Docker's `0:0` default. A pure pull leaves no
    // `snapshots/` directory at all, so use a store that never wrote one.
    let bare = TempDir::new_in("/var/tmp").expect("real disk store");
    let bare_engine = BtrfsEngine::new(slipfork_world::StoreLayout::new(bare.path()));
    let (layout, head) = sealed_head_without_record(&bare, 5, "cached");
    assert!(
        !layout.snapshots_root().is_dir(),
        "the fixture must model a store with no node records"
    );
    assert_eq!(
        super::sealed_identity(&layout, &bare_engine, &head)
            .expect("a record-less cached-from image keeps the root default"),
        slipfork_world::Identity::ROOT
    );

    // A Dockerfile head cached on a 0-miss rebuild still has its step record: if GC swept the
    // image record it fails closed rather than booting root over an explicit `USER`.
    let (layout, head) = sealed_head(&disk, 6, None, "cached");
    let error = super::sealed_identity(&layout, &engine, &head)
        .expect_err("a recorded cached head without an image record must fail closed");
    assert!(
        error.cause().contains("no recorded `USER`"),
        "{}",
        error.cause()
    );

    // A cached head that does carry an explicit `USER` still fails closed on a missing account.
    let (layout, head) = sealed_head(&disk, 7, Some("ghost"), "cached");
    let error = super::sealed_identity(&layout, &engine, &head)
        .expect_err("an unresolvable USER must fail closed even on a cached head");
    assert!(
        error.cause().contains("`ghost`") && error.cause().contains("/etc/passwd"),
        "the cause names the account and its file: {}",
        error.cause()
    );
    assert!(
        error.remedy().contains("useradd"),
        "the remedy names the fix: {}",
        error.remedy()
    );
}
