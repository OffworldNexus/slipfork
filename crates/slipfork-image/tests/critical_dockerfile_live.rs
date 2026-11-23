//! Protected live suite for the native multi-stage `Dockerfile` builder (`--features live`).
//!
//! One journey: a real loopback `Distribution v2` base image is pulled into a real Btrfs
//! subvolume, two stages run through real `crun` pods, `RUN --mount=type=cache` persists across
//! builds, an identical rebuild is a sealed-step cache hit, and every produced subvolume is
//! read-only. On a non-Btrfs host or a host without `crun` the suite FAILS CLOSED instead of
//! skipping, because a skipped CoW build suite hides broken snapshots forever.

#[path = "support/mod.rs"]
mod support;

use std::fs;
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use slipfork_image::{
    BuildArgs, BuildRequest, Command, Credentials, ImageLayout, NativeImageBuilder, OciPuller,
    Port, Protocol, RegistryScheme, StepHost, StepLayout,
};
use slipfork_world::{BtrfsEngine, Dirtiness, StoreLayout, WorldEngine, is_subvolume_path};

/// The last few KiB of a build log, so a failing `crun` step is never silent.
fn output_tail(log: &[u8]) -> String {
    const LIMIT: usize = 4096;
    let start = log.len().saturating_sub(LIMIT);
    String::from_utf8_lossy(&log[start..]).trim().to_owned()
}

#[test]
fn native_dockerfile_multistage_build_lifecycle() {
    let crun = crun_path();
    let run = support::live_run_root("dockerfile");
    // SAFETY: `getuid`/`getgid` only read the caller's identity and cannot fail.
    let uid = unsafe { libc::getuid() };
    // SAFETY: as above; `getgid` has no preconditions.
    let gid = unsafe { libc::getgid() };

    let store = StoreLayout::new(&run);
    let engine = Arc::new(BtrfsEngine::new(store.clone()));
    engine.ensure_store().expect("create the Btrfs image store");
    let images = ImageLayout::new(store.images_root());
    let steps = StepLayout::new(store.images_root());
    let scratch = run.join("bundles");

    let fixture = support::Fixture::start();
    let arch = support::host_architecture();
    let puller = OciPuller::new(
        Arc::clone(&engine) as Arc<dyn WorldEngine>,
        images,
        Credentials::Anonymous,
        arch,
        "slipfork-tests/0.1".to_owned(),
    )
    .with_scheme(RegistryScheme::Http);
    let builder = NativeImageBuilder::new(
        Arc::clone(&engine) as Arc<dyn WorldEngine>,
        puller,
        steps.clone(),
        arch,
        StepHost::new(crun, "/etc/resolv.conf", &scratch, uid, gid),
    );

    // The base image is a runnable loopback image: the host shell plus its loader closure.
    let base = fixture.reference_text("library/shell");
    let context = run.join("context");
    fs::create_dir_all(&context).expect("create the build context");
    fs::write(context.join("note.txt"), "note-v1\n").expect("write an included file");
    fs::write(context.join("owned.txt"), "owned\n").expect("write the chown file");
    fs::write(context.join("secret.env"), "TOKEN=leak\n").expect("write the ignored file");
    fs::write(context.join("debug.log"), "noise\n").expect("write an ignored log");
    fs::write(context.join(".dockerignore"), "secret.env\n*.log\n").expect("write .dockerignore");
    let dockerfile = context.join("Dockerfile");
    fs::write(&dockerfile, dockerfile_source(&base, "run-one", uid, gid))
        .expect("write the Dockerfile");

    let request = BuildRequest {
        context: context.clone(),
        dockerfile: dockerfile.clone(),
        args: BuildArgs::new(),
        target: None,
    };
    let mut first_log = Vec::new();
    let first = builder
        .build(&request, &mut first_log)
        .unwrap_or_else(|error| {
            panic!(
                "first multi-stage build: {error}\nbuild output tail:\n{}",
                output_tail(&first_log)
            )
        });
    assert_eq!(first.cache_hits, 0, "a cold build serves no sealed step");
    assert_eq!(first.cache_misses, 5, "five filesystem instructions");
    let cache_file = steps.cache_mount("/root/.cache/x").join("out.txt");
    assert_eq!(support::read_text(&cache_file), "run-one\n");
    let blobs_after_first = fixture.blob_requests();
    assert!(
        blobs_after_first >= 1,
        "the first build must pull the loopback base image"
    );

    // Second build: the cache mount survives, so the RUN appends instead of starting empty.
    fs::write(&dockerfile, dockerfile_source(&base, "run-two", uid, gid))
        .expect("rewrite the Dockerfile");
    let mut second_log = Vec::new();
    let second = builder
        .build(&request, &mut second_log)
        .unwrap_or_else(|error| {
            panic!(
                "second build: {error}\nbuild output tail:\n{}",
                output_tail(&second_log)
            )
        });
    assert_eq!(second.cache_misses, 1, "only the changed RUN re-executes");
    assert_eq!(second.cache_hits, 4, "the final stage steps are reused");
    assert_eq!(
        support::read_text(&cache_file),
        "run-one\nrun-two\n",
        "`RUN --mount=type=cache` must persist for a later build"
    );

    // Third build: byte-identical to the second, so every step is a sealed cache hit.
    let mut cached_log = Vec::new();
    let cached = builder
        .build(&request, &mut cached_log)
        .unwrap_or_else(|error| {
            panic!(
                "cached build: {error}\nbuild output tail:\n{}",
                output_tail(&cached_log)
            )
        });
    assert_eq!(cached.cache_misses, 0, "an identical rebuild never re-runs");
    assert_eq!(cached.cache_hits, 5, "every step must hit the sealed cache");
    // 2-vCPU CI scheduler jitter on a loop-backed ext4 image can triple wall-clock; 250ms still
    // catches a rebuild that re-executes a step instead of hitting the sealed cache.
    assert!(
        cached.build_time < Duration::from_millis(250),
        "a sealed-cache rebuild must take < 250ms, took {:?}",
        cached.build_time
    );
    assert_eq!(
        fixture.blob_requests(),
        blobs_after_first,
        "a cached base image must not re-download a blob"
    );
    assert_eq!(support::read_text(&cache_file), "run-one\nrun-two\n");

    // Metadata from `ENV`/`WORKDIR`/`USER`/`EXPOSE`/`CMD`.
    assert_eq!(
        cached.config.env.get("APP_MODE").map(String::as_str),
        Some("production")
    );
    assert_eq!(cached.config.workdir, "/app");
    let user = format!("{uid}:{gid}");
    assert_eq!(cached.config.user.as_deref(), Some(user.as_str()));
    assert!(
        cached.config.exposed.contains(&Port {
            number: 8080,
            protocol: Protocol::Tcp,
        }),
        "expected EXPOSE 8080/tcp, saw {:?}",
        cached.config.exposed
    );
    assert!(cached.config.exposed.contains(&Port {
        number: 9090,
        protocol: Protocol::Udp,
    }));
    assert_eq!(
        cached.config.cmd,
        Some(Command::Exec(vec![
            "/app/run".to_owned(),
            "--serve".to_owned()
        ]))
    );

    // Rootfs contents: `COPY --from`, `COPY --chown`, `COPY .` and `.dockerignore`.
    let rootfs = &cached.rootfs;
    assert_eq!(
        support::read_text(&rootfs.join("app/from-builder.txt")),
        "built\n",
        "`COPY --from=builder` must carry the builder output"
    );
    assert_eq!(
        support::read_text(&rootfs.join("app/note.txt")),
        "note-v1\n",
        "`COPY . /app` must carry the included context file"
    );
    assert!(
        !rootfs.join("app/secret.env").exists(),
        "`.dockerignore` must keep the planted secret out of the image"
    );
    assert!(!rootfs.join("app/debug.log").exists());
    let owned = fs::metadata(rootfs.join("owned.txt")).expect("stat the chowned file");
    assert_eq!(
        (owned.uid(), owned.gid()),
        (uid, gid),
        "`COPY --chown` must apply the numeric owner"
    );
    assert!(rootfs.join("app").is_dir(), "WORKDIR must materialize /app");

    // Every produced step and the final rootfs are sealed read-only subvolumes.
    let mut sealed = 0usize;
    for entry in fs::read_dir(steps.steps()).expect("read the step cache") {
        let path = entry.expect("step entry").path();
        if path
            .extension()
            .is_some_and(|extension| extension == "json")
        {
            continue;
        }
        assert!(
            engine.is_sealed(&path).expect("probe the step seal"),
            "step `{}` must be sealed",
            path.display()
        );
        let write = fs::write(path.join("blocked"), b"x").expect_err("a sealed step is read-only");
        assert_eq!(
            write.raw_os_error(),
            Some(libc::EROFS),
            "expected EROFS from BTRFS_SUBVOL_RDONLY on `{}`",
            path.display()
        );
        sealed += 1;
    }
    assert_eq!(sealed, 6, "five steps plus the second RUN variant");
    assert!(engine.is_sealed(rootfs).expect("probe the final seal"));

    // Cleanup: reaping the worlds keeps the scratch root free of open process groups and
    // sealed subvolumes that `remove_dir_all` cannot delete.
    drop(fixture);
    destroy_children(&engine, &steps.steps());
    destroy_children(&engine, &store.images_root().join("oci"));
    let _ = fs::remove_dir_all(&run);
}

/// Real `--target` semantics on sealed subvolumes: a `dev` target stops after its stage, so the
/// later `prod` stage's always-failing `RUN` is never reached; targeting `prod` fails on that
/// `RUN`; and an unknown alias is refused before any pull or step.
#[test]
fn a_target_stage_never_reaches_a_later_failing_stage() {
    let crun = crun_path();
    let run = support::live_run_root("dockerfile-target");
    // SAFETY: `getuid`/`getgid` only read the caller's identity and cannot fail.
    let uid = unsafe { libc::getuid() };
    // SAFETY: as above; `getgid` has no preconditions.
    let gid = unsafe { libc::getgid() };

    let store = StoreLayout::new(&run);
    let engine = Arc::new(BtrfsEngine::new(store.clone()));
    engine.ensure_store().expect("create the Btrfs image store");
    let images = ImageLayout::new(store.images_root());
    let steps = StepLayout::new(store.images_root());
    let scratch = run.join("bundles");

    let fixture = support::Fixture::start();
    let arch = support::host_architecture();
    let puller = OciPuller::new(
        Arc::clone(&engine) as Arc<dyn WorldEngine>,
        images,
        Credentials::Anonymous,
        arch,
        "slipfork-tests/0.1".to_owned(),
    )
    .with_scheme(RegistryScheme::Http);
    let builder = NativeImageBuilder::new(
        Arc::clone(&engine) as Arc<dyn WorldEngine>,
        puller,
        steps.clone(),
        arch,
        StepHost::new(crun, "/etc/resolv.conf", &scratch, uid, gid),
    );

    let base = fixture.reference_text("library/shell");
    let context = run.join("context");
    fs::create_dir_all(&context).expect("create the build context");
    let dockerfile = context.join("Dockerfile");
    fs::write(
        &dockerfile,
        format!(
            "FROM {base} AS dev\n\
             ENV APP_MODE=dev\n\
             FROM {base} AS prod\n\
             RUN sh -c 'exit 7'\n"
        ),
    )
    .expect("write the two-stage Dockerfile");

    let dev_request = BuildRequest {
        context: context.clone(),
        dockerfile: dockerfile.clone(),
        args: BuildArgs::new(),
        target: Some("dev".to_owned()),
    };
    let mut dev_log = Vec::new();
    let dev = builder
        .build(&dev_request, &mut dev_log)
        .unwrap_or_else(|error| {
            panic!(
                "`--target dev` must never reach the prod RUN: {error}\nbuild output tail:\n{}",
                output_tail(&dev_log)
            )
        });
    assert_eq!(dev.cache_misses, 0, "the dev stage is metadata only");

    let prod_request = BuildRequest {
        target: Some("prod".to_owned()),
        ..dev_request.clone()
    };
    let mut prod_log = Vec::new();
    let error = builder
        .build(&prod_request, &mut prod_log)
        .expect_err("the prod stage's failing RUN must fail the build");
    assert!(
        matches!(error, slipfork_image::Error::StepFailed { line: 4, .. }),
        "unexpected error: {error}\nbuild output tail:\n{}",
        output_tail(&prod_log)
    );

    let blobs_before = fixture.blob_requests();
    let unknown = BuildRequest {
        target: Some("staging".to_owned()),
        ..dev_request
    };
    let error = builder
        .build(&unknown, &mut Vec::new())
        .expect_err("an absent target alias must fail");
    assert!(
        matches!(error, slipfork_image::Error::UnknownStage { ref name } if name == "staging"),
        "unexpected error: {error}"
    );
    assert_eq!(
        fixture.blob_requests(),
        blobs_before,
        "an unknown alias must fail before any pull"
    );

    drop(fixture);
    destroy_children(&engine, &steps.steps());
    destroy_children(&engine, &store.images_root().join("oci"));
    let _ = fs::remove_dir_all(&run);
}

/// The `Forkfile` compiler is the canonical owner of secret taint: a secret can never become a
/// build-time `arg`, and [`BuildArgs`] only accepts plain strings.
#[test]
fn forkfile_compiler_rejects_secret_build_args() {
    let source = "# forkfile: 0.1\n\nimage backend . {\n  arg X = secret(TOKEN)\n}\n";
    let document =
        slipfork_forkfile::parse(source).expect("the fixture is syntactically valid Forkfile");
    let (_, diagnostics) =
        slipfork_forkfile::check(&document).expect_err("a secret build-time arg must be rejected");
    assert!(
        diagnostics
            .items
            .iter()
            .any(|diagnostic| diagnostic.message.contains("bake a secret")),
        "expected the canonical secret-taint diagnostic, saw {:?}",
        diagnostics.items
    );

    let mut args = BuildArgs::new();
    assert_eq!(args.iter().count(), 0, "BuildArgs starts empty");
    args.declare("X", "plain");
    assert_eq!(args.get("X"), Some("plain"));
}

fn dockerfile_source(base: &str, marker: &str, uid: u32, gid: u32) -> String {
    format!(
        "FROM {base} AS builder\n\
         RUN --mount=type=cache,target=/root/.cache/x <<EOF\n\
         echo {marker} >> /root/.cache/x/out.txt\n\
         echo built > /root/out.txt\n\
         EOF\n\
         FROM {base} AS final\n\
         COPY --from=builder /root/out.txt /app/from-builder.txt\n\
         COPY --chown={uid}:{gid} owned.txt /owned.txt\n\
         COPY . /app\n\
         WORKDIR /app\n\
         ENV APP_MODE=production\n\
         USER {uid}:{gid}\n\
         EXPOSE 8080 9090/udp\n\
         CMD [\"/app/run\", \"--serve\"]\n"
    )
}

/// `crun` is resolved once by the test (a composition root) and injected into the builder.
fn crun_path() -> PathBuf {
    let remedy = "next: install crun (`apt install crun`, `dnf install crun`, `pacman -S crun`) or export SLIPFORK_TEST_CRUN=/path/to/crun";
    if let Some(path) = std::env::var_os("SLIPFORK_TEST_CRUN") {
        let path = PathBuf::from(path);
        assert!(
            path.is_file(),
            "fail closed: SLIPFORK_TEST_CRUN=`{}` is not an executable file.\n{remedy}",
            path.display()
        );
        return path;
    }
    let Some(search) = std::env::var_os("PATH") else {
        panic!("fail closed: `crun` is not installed and PATH is unset.\n{remedy}");
    };
    for directory in std::env::split_paths(&search) {
        let candidate = directory.join("crun");
        if candidate.is_file() {
            return candidate;
        }
    }
    panic!("fail closed: `crun` is not installed, so `RUN` cannot execute.\n{remedy}");
}

/// Destroys every sealed subvolume directly under `root`; plain directories are left in place.
fn destroy_children(engine: &BtrfsEngine, root: &Path) {
    let Ok(entries) = fs::read_dir(root) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if is_subvolume_path(&path).unwrap_or(false) {
            let _ = engine.destroy(&path, Dirtiness::Dirty, true);
        }
    }
}
