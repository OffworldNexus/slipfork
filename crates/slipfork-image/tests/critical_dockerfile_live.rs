//! Protected live suite for the native multi-stage `Dockerfile` builder (`--features live`).
//!
//! One journey: a real loopback `Distribution v2` base image is pulled into a real Btrfs
//! subvolume, two stages run through real `crun` pods, `RUN --mount=type=cache` persists across
//! builds, an identical rebuild is a sealed-step cache hit, and every produced subvolume is
//! read-only. On a non-Btrfs host or a host without `crun` the suite FAILS CLOSED instead of
//! skipping, because a skipped CoW build suite hides broken snapshots forever.

#[path = "support/mod.rs"]
mod support;

use std::collections::BTreeMap;
use std::fs;
use std::io::{Read, Write};
use std::net::TcpListener;
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use slipfork_image::{
    BuildArgs, BuildEvent, BuildRequest, CacheMounts, Command, CrunStepRuntime, ExternalImage,
    ImageConfig, ImageLayout, ImageReference, NativeImageBuilder, OciPuller, Port, Protocol,
    RegistryCredentials, RegistryScheme, RunSpec, StepHost, StepLog, Stream, external_references,
};
use slipfork_world::{BtrfsEngine, Cache, Dirtiness, StoreLayout, WorldEngine, is_subvolume_path};

/// The last few KiB of a build's streamed output, so a failing `crun` step is never silent.
fn output_tail(log: &[(Stream, String)]) -> String {
    const LIMIT: usize = 4096;
    let joined = log
        .iter()
        .map(|(_, line)| line.as_str())
        .collect::<Vec<_>>()
        .join("\n");
    let start = joined.len().saturating_sub(LIMIT);
    joined.get(start..).unwrap_or(&joined).to_owned()
}

/// Child output without a terminal escape sequence or a trailing CR, so `Get:`/`Hit:` matching is
/// exact even when a base exports `APT::Color`.
fn clean_line(line: &str) -> String {
    let mut out = String::with_capacity(line.len());
    let mut chars = line.chars();
    while let Some(ch) = chars.next() {
        if ch == '\x1b' {
            if chars.next() == Some('[') {
                // Skip CSI parameter/intermediate bytes, stopping after the final byte.
                for next in chars.by_ref() {
                    if !('\u{20}'..='\u{3f}').contains(&next) {
                        break;
                    }
                }
            }
            continue;
        }
        if ch != '\r' {
            out.push(ch);
        }
    }
    out
}

/// Runs one build against a bound [`StepLog`], keeping every `(stream, line)` for failure tails.
fn build_capture(
    builder: &NativeImageBuilder,
    request: &BuildRequest,
) -> (
    slipfork_image::Result<slipfork_image::BuildReport>,
    Vec<(Stream, String)>,
) {
    let mut streamed: Vec<(Stream, String)> = Vec::new();
    let outcome = {
        let mut events = |event: BuildEvent<'_>| {
            if let BuildEvent::Line { stream, text } = event {
                streamed.push((stream, text.to_owned()));
            }
        };
        let mut log = StepLog::with_events(&mut events);
        builder.build(request, &mut log, 1_000)
    };
    (outcome, streamed)
}

/// The payload the loopback URL server serves to `ADD <url>`.
const INSTALL_BODY: &[u8] = b"#!/bin/sh\necho added\n";

/// A loopback HTTP server for `ADD <url>`: real sockets, real bytes, no internet.
struct UrlServer {
    addr: std::net::SocketAddr,
    stop: Arc<AtomicBool>,
    serving: Option<thread::JoinHandle<()>>,
}

impl UrlServer {
    fn start(body: &'static [u8]) -> Self {
        let listener = TcpListener::bind(("127.0.0.1", 0)).expect("bind the loopback URL server");
        let addr = listener
            .local_addr()
            .expect("read the loopback URL address");
        listener
            .set_nonblocking(true)
            .expect("make the URL listener non-blocking");
        let stop = Arc::new(AtomicBool::new(false));
        let flag = Arc::clone(&stop);
        let serving = thread::spawn(move || {
            while !flag.load(Ordering::Relaxed) {
                match listener.accept() {
                    Ok((mut stream, _)) => {
                        let mut request = Vec::new();
                        let mut buffer = [0u8; 1024];
                        while !request.windows(4).any(|window| window == b"\r\n\r\n") {
                            let read = stream.read(&mut buffer).unwrap_or(0);
                            if read == 0 {
                                break;
                            }
                            request.extend_from_slice(&buffer[..read]);
                        }
                        let head = format!(
                            "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                            body.len()
                        );
                        let _ = stream.write_all(head.as_bytes());
                        let _ = stream.write_all(body);
                        let _ = stream.flush();
                    }
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        thread::sleep(Duration::from_millis(5));
                    }
                    Err(_) => break,
                }
            }
        });
        Self {
            addr,
            stop,
            serving: Some(serving),
        }
    }

    /// `http://127.0.0.1:<port><path>`.
    fn url(&self, path: &str) -> String {
        format!("http://{}{path}", self.addr)
    }
}

impl Drop for UrlServer {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(handle) = self.serving.take() {
            let _ = handle.join();
        }
    }
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
    let scratch = run.join("bundles");

    let fixture = support::Fixture::start();
    let arch = support::host_architecture();
    let puller = OciPuller::new(
        Arc::clone(&engine) as Arc<dyn WorldEngine>,
        images,
        RegistryCredentials::default(),
        arch,
        "slipfork-tests/0.1".to_owned(),
    )
    .with_scheme(RegistryScheme::Http);
    let builder = NativeImageBuilder::new(
        Arc::clone(&engine) as Arc<dyn WorldEngine>,
        puller,
        store.clone(),
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
    let url_server = UrlServer::start(INSTALL_BODY);
    let install_url = url_server.url("/install.sh");
    let dockerfile = context.join("Dockerfile");
    fs::write(
        &dockerfile,
        dockerfile_source(&base, &install_url, "run-one", uid, gid),
    )
    .expect("write the Dockerfile");

    let request = BuildRequest {
        context: context.clone(),
        dockerfile: dockerfile.clone(),
        args: BuildArgs::new(),
        target: None,
        externals: BTreeMap::new(),
    };
    let (first, first_log) = build_capture(&builder, &request);
    let first = first.unwrap_or_else(|error| {
        panic!(
            "first multi-stage build: {error}\nbuild output tail:\n{}",
            output_tail(&first_log)
        )
    });
    assert_eq!(first.cache_hits, 0, "a cold build serves no sealed step");
    assert_eq!(first.cache_misses, 7, "seven filesystem instructions");
    let cache_file = CacheMounts::new(store.images_root())
        .dir("/root/.cache/x")
        .join("out.txt");
    assert_eq!(support::read_text(&cache_file), "run-one\n");
    let blobs_after_first = fixture.blob_requests();
    assert!(
        blobs_after_first >= 1,
        "the first build must pull the loopback base image"
    );

    // Second build: the cache mount survives, so the RUN appends instead of starting empty.
    fs::write(
        &dockerfile,
        dockerfile_source(&base, &install_url, "run-two", uid, gid),
    )
    .expect("rewrite the Dockerfile");
    let (second, second_log) = build_capture(&builder, &request);
    let second = second.unwrap_or_else(|error| {
        panic!(
            "second build: {error}\nbuild output tail:\n{}",
            output_tail(&second_log)
        )
    });
    assert_eq!(second.cache_misses, 1, "only the changed RUN re-executes");
    assert_eq!(
        second.cache_hits, 6,
        "every other step hits the sealed cache"
    );
    assert_eq!(
        support::read_text(&cache_file),
        "run-one\nrun-two\n",
        "`RUN --mount=type=cache` must persist for a later build"
    );

    // Third build: byte-identical to the second, so every step is a sealed cache hit.
    let (cached, cached_log) = build_capture(&builder, &request);
    let cached = cached.unwrap_or_else(|error| {
        panic!(
            "cached build: {error}\nbuild output tail:\n{}",
            output_tail(&cached_log)
        )
    });
    assert_eq!(cached.cache_misses, 0, "an identical rebuild never re-runs");
    assert_eq!(cached.cache_hits, 7, "every step must hit the sealed cache");
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
    let install = rootfs.join("opt/data/install.sh");
    assert_eq!(
        fs::read(&install).expect("read the ADD url payload"),
        INSTALL_BODY,
        "`ADD <url>` must stream the loopback payload"
    );
    assert_eq!(
        fs::metadata(&install)
            .expect("stat the ADD url payload")
            .mode()
            & 0o777,
        0o644,
        "`ADD <url>` lands 0644"
    );

    // Every produced step and the final rootfs are sealed read-only subvolumes.
    let mut sealed = 0usize;
    for entry in fs::read_dir(store.snapshots_root()).expect("read the snapshot store") {
        let path = entry.expect("step entry").path();
        if !is_node_dir(&path) {
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
    assert_eq!(sealed, 9, "eight step variants plus the base node");
    assert!(engine.is_sealed(rootfs).expect("probe the final seal"));

    // Cleanup: reaping the worlds keeps the scratch root free of open process groups and
    // sealed subvolumes that `remove_dir_all` cannot delete.
    drop(fixture);
    destroy_children(&engine, &store.snapshots_root());
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
    let scratch = run.join("bundles");

    let fixture = support::Fixture::start();
    let arch = support::host_architecture();
    let puller = OciPuller::new(
        Arc::clone(&engine) as Arc<dyn WorldEngine>,
        images,
        RegistryCredentials::default(),
        arch,
        "slipfork-tests/0.1".to_owned(),
    )
    .with_scheme(RegistryScheme::Http);
    let builder = NativeImageBuilder::new(
        Arc::clone(&engine) as Arc<dyn WorldEngine>,
        puller,
        store.clone(),
        arch,
        StepHost::new(&crun, "/etc/resolv.conf", &scratch, uid, gid),
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
        externals: BTreeMap::new(),
    };
    let (dev, dev_log) = build_capture(&builder, &dev_request);
    let dev = dev.unwrap_or_else(|error| {
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
    let (error, prod_log) = build_capture(&builder, &prod_request);
    let error = error.expect_err("the prod stage's failing RUN must fail the build");
    assert!(
        matches!(error, slipfork_image::Error::StepFailed { line: 4, .. }),
        "unexpected error: {error}\nbuild output tail:\n{}",
        output_tail(&prod_log)
    );

    // Rebuilding after a failed/interrupted prod `RUN` must reclaim any stale `sf-<hash>` state in
    // `crun` instead of failing with `container 'sf-...' already exists`.
    let (retry_error, retry_log) = build_capture(&builder, &prod_request);
    let retry_error = retry_error.expect_err(
        "retrying the failing prod stage still fails on line 4, never `already exists`",
    );
    assert!(
        matches!(
            retry_error,
            slipfork_image::Error::StepFailed { line: 4, .. }
        ) && !retry_error.to_string().contains("already exists"),
        "unexpected retry error: {retry_error}\nbuild output tail:\n{}",
        output_tail(&retry_log)
    );
    // `crun` roots its state at `$XDG_RUNTIME_DIR/crun`, falling back to `/run/user/{uid}` only
    // when the variable is unset, so the planted state must follow the same resolution.
    let runtime_user_dir = std::env::var_os("XDG_RUNTIME_DIR")
        .filter(|dir| !dir.is_empty())
        .map_or_else(|| PathBuf::from(format!("/run/user/{uid}")), PathBuf::from);
    if runtime_user_dir.is_dir() {
        let pid = std::process::id();
        let stale_id = format!("sf-{pid}-0123456789abcdef");
        let foreign_id = format!("sf-{}-0123456789abcdef", pid.wrapping_add(100_000));
        let stale_state = runtime_user_dir.join("crun").join(&stale_id);
        let foreign_state = runtime_user_dir.join("crun").join(&foreign_id);
        fs::create_dir_all(&stale_state).expect("plant own stale crun container dir");
        fs::create_dir_all(&foreign_state).expect("plant concurrent foreign-pid crun dir");
        let step_rootfs = run.join("stale-step-rootfs");
        engine
            .clone_subvolume(&dev.rootfs, &step_rootfs)
            .expect("clone writable step rootfs");
        let step_runtime =
            CrunStepRuntime::new(StepHost::new(&crun, "/etc/resolv.conf", &scratch, uid, gid));
        let mut step_log: Vec<(Stream, String)> = Vec::new();
        let outcome = {
            let mut events = |event: BuildEvent<'_>| {
                if let BuildEvent::Line { stream, text } = event {
                    step_log.push((stream, text.to_owned()));
                }
            };
            let mut log = StepLog::with_events(&mut events);
            step_runtime.run(
                &stale_id,
                &step_rootfs,
                &ImageConfig::default(),
                &RunSpec {
                    mounts: Vec::new(),
                    command: Command::Shell("echo reclaimed > /reclaimed.txt".to_owned()),
                },
                &[],
                1,
                &mut log,
            )
        };
        outcome.unwrap_or_else(|err| {
            panic!(
                "CrunStepRuntime must evict stale container `{stale_id}` before run: {err}\n{}",
                output_tail(&step_log)
            )
        });
        assert!(
            runtime_user_dir.join("crun").is_dir(),
            "crun must root its state at `{}`",
            runtime_user_dir.join("crun").display()
        );
        assert_eq!(
            support::read_text(&step_rootfs.join("reclaimed.txt")),
            "reclaimed\n"
        );
        assert!(
            !stale_state.exists(),
            "step cleanup must remove `{}` after completion",
            stale_state.display()
        );
        assert!(
            foreign_state.exists(),
            "process-scoped cleanup must leave another process's `{}` untouched",
            foreign_state.display()
        );
        let _ = fs::remove_dir_all(&foreign_state);
        let _ = engine.destroy(&step_rootfs, Dirtiness::Dirty, true);
    } else {
        panic!(
            "fail closed: crun runtime root `{}` is missing, so stale-state reclamation was never asserted.\nnext: run `make live-test` (it creates XDG_RUNTIME_DIR on tmpfs)",
            runtime_user_dir.display()
        );
    }

    let blobs_before = fixture.blob_requests();
    let unknown = BuildRequest {
        target: Some("staging".to_owned()),
        ..dev_request
    };
    let (error, _) = build_capture(&builder, &unknown);
    let error = error.expect_err("an absent target alias must fail");
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
    destroy_children(&engine, &store.snapshots_root());
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

/// One external-image journey on a single store boot: `COPY --from` by image/index/name, a moved
/// external digest invalidating the sealed copy step, and the owner's real `uv` reference through
/// `crun`.
#[test]
fn external_copy_from_image_lifecycle() {
    let crun = crun_path();
    // SAFETY: `getuid`/`getgid` only read the caller's identity and cannot fail.
    let uid = unsafe { libc::getuid() };
    // SAFETY: as above; `getgid` has no preconditions.
    let gid = unsafe { libc::getgid() };
    let run = support::live_run_root("dockerfile-external");
    let store = StoreLayout::new(&run);
    let engine = Arc::new(BtrfsEngine::new(store.clone()));
    engine.ensure_store().expect("create the Btrfs image store");
    let scratch = run.join("bundles");
    let fixture = support::Fixture::start();
    let arch = support::host_architecture();
    let loopback = OciPuller::new(
        Arc::clone(&engine) as Arc<dyn WorldEngine>,
        ImageLayout::new(store.images_root()),
        RegistryCredentials::default(),
        arch,
        "slipfork-tests/0.1".to_owned(),
    )
    .with_scheme(RegistryScheme::Http);

    // The `FROM` base is `library/demo` (has `bin/tool`, no `/bin/sh`), so resolving a `--from`
    // external to the base stage cannot accidentally produce the copied bytes.
    let demo_ref = fixture.reference_text("library/demo");
    let shell_ref = fixture.reference_text("library/shell");
    let demo = loopback
        .pull_into(&fixture.reference("library/demo"), &mut StepLog::discard())
        .expect("pull the loopback demo rootfs");
    let shell = loopback
        .pull_into(&fixture.reference("library/shell"), &mut StepLog::discard())
        .expect("pull the loopback shell rootfs");
    let shell_bytes = fs::read(shell.rootfs.join("bin/sh")).expect("read the external `/bin/sh`");
    let shell_image = ExternalImage {
        rootfs: shell.rootfs,
        digest: shell.digest,
    };
    let demo_image = ExternalImage {
        rootfs: demo.rootfs,
        digest: demo.digest,
    };

    let builder = NativeImageBuilder::new(
        Arc::clone(&engine) as Arc<dyn WorldEngine>,
        loopback,
        store.clone(),
        arch,
        StepHost::new(&crun, "/etc/resolv.conf", &scratch, uid, gid),
    );
    let context = run.join("context");
    fs::create_dir_all(&context).expect("create the build context");
    let dockerfile = context.join("Dockerfile");

    // 1. image reference, stage index, and stage name all reach the pulled external rootfs.
    fs::write(
        &dockerfile,
        format!(
            "FROM {demo_ref} AS first\n\
             COPY --from={shell_ref} /bin/sh /bin/external-sh\n\
             FROM {demo_ref} AS second\n\
             COPY --from=0 /bin/external-sh /bin/by-index\n\
             COPY --from=first /bin/external-sh /bin/by-name\n"
        ),
    )
    .expect("write the external Dockerfile");
    let mut stage_externals = BTreeMap::new();
    stage_externals.insert(shell_ref.clone(), shell_image.clone());
    let request = BuildRequest {
        context: context.clone(),
        dockerfile: dockerfile.clone(),
        args: BuildArgs::new(),
        target: None,
        externals: stage_externals,
    };
    let (report, log) = build_capture(&builder, &request);
    let report = report.unwrap_or_else(|error| {
        panic!(
            "external COPY build: {error}\nbuild output tail:\n{}",
            output_tail(&log)
        )
    });
    // `bin/external-sh` exists only in stage `first`, so the final `second` rootfs carries just
    // the two stage copies made by that stage.
    for artifact in ["bin/by-index", "bin/by-name"] {
        let path = report.rootfs.join(artifact);
        assert!(path.is_file(), "`{artifact}` must exist");
        assert!(
            fs::metadata(&path).expect("stat the artifact").mode() & 0o111 != 0,
            "`{artifact}` must stay executable"
        );
        assert_eq!(
            fs::read(&path).expect("read the copied artifact"),
            shell_bytes,
            "`{artifact}` must be the external rootfs's `/bin/sh` bytes"
        );
    }
    let (repeat, repeat_log) = build_capture(&builder, &request);
    let repeat = repeat.unwrap_or_else(|error| {
        panic!(
            "cached external build: {error}\nbuild output tail:\n{}",
            output_tail(&repeat_log)
        )
    });
    assert_eq!(repeat.cache_misses, 0, "a repeat build is all sealed hits");
    assert_eq!(
        repeat.cache_hits, 3,
        "three external/stage copies are sealed"
    );
    // Targeting `first` reuses the sealed steps of the same directory and leaves the external
    // copy in the rootfs of the stage that actually ran it.
    let first_stage_request = BuildRequest {
        target: Some("first".to_owned()),
        ..request.clone()
    };
    let (first_stage, first_stage_log) = build_capture(&builder, &first_stage_request);
    let first_stage = first_stage.unwrap_or_else(|error| {
        panic!(
            "`--target first` build: {error}\nbuild output tail:\n{}",
            output_tail(&first_stage_log)
        )
    });
    let external = first_stage.rootfs.join("bin/external-sh");
    assert!(
        external.is_file(),
        "`bin/external-sh` must exist in the `first` stage rootfs"
    );
    assert!(
        fs::metadata(&external)
            .expect("stat the external artifact")
            .mode()
            & 0o111
            != 0,
        "`bin/external-sh` must stay executable"
    );
    assert_eq!(
        fs::read(&external).expect("read the external artifact"),
        shell_bytes,
        "`bin/external-sh` must be the external rootfs's `/bin/sh` bytes"
    );
    let empty = BuildRequest {
        externals: BTreeMap::new(),
        ..request.clone()
    };
    let (error, _) = build_capture(&builder, &empty);
    let error = error.expect_err("an external COPY with no pulled rootfs must fail");
    assert!(
        matches!(error, slipfork_image::Error::ExternalImageMissing { ref reference } if reference == &shell_ref),
        "unexpected error: {error}"
    );

    // 2. A moved external digest must bust the sealed COPY step, not serve a stale `/app/bin`.
    fs::write(
        &dockerfile,
        format!("FROM {demo_ref}\nCOPY --from=external /bin /app/bin\n"),
    )
    .expect("rewrite the digest-salt Dockerfile");
    let mut shell_map = BTreeMap::new();
    shell_map.insert("external".to_owned(), shell_image.clone());
    let shell_request = BuildRequest {
        context: context.clone(),
        dockerfile: dockerfile.clone(),
        args: BuildArgs::new(),
        target: None,
        externals: shell_map,
    };
    let (shell_build, shell_log) = build_capture(&builder, &shell_request);
    let shell_build = shell_build.unwrap_or_else(|error| {
        panic!(
            "shell-salt build: {error}\nbuild output tail:\n{}",
            output_tail(&shell_log)
        )
    });
    assert!(
        shell_build.rootfs.join("app/bin/sh").is_file(),
        "the shell rootfs must seed `/app/bin/sh`"
    );
    let mut demo_map = BTreeMap::new();
    demo_map.insert("external".to_owned(), demo_image);
    let demo_request = BuildRequest {
        externals: demo_map,
        ..shell_request.clone()
    };
    let (demo_build, demo_log) = build_capture(&builder, &demo_request);
    let demo_build = demo_build.unwrap_or_else(|error| {
        panic!(
            "demo-salt build: {error}\nbuild output tail:\n{}",
            output_tail(&demo_log)
        )
    });
    assert!(
        demo_build.cache_misses >= 1,
        "a moved external digest must re-execute the sealed COPY"
    );
    assert!(
        demo_build.rootfs.join("app/bin/tool").is_file(),
        "the demo rootfs must seed `/app/bin/tool`"
    );
    assert!(
        !demo_build.rootfs.join("app/bin/sh").exists(),
        "the stale shell payload must be replaced, not merged"
    );

    // 3. The owner's real Dockerfile: pull `uv` by reference over HTTPS, then run it via `crun`.
    let https = OciPuller::new(
        Arc::clone(&engine) as Arc<dyn WorldEngine>,
        ImageLayout::new(store.images_root()),
        RegistryCredentials::default(),
        arch,
        "slipfork-tests/0.1".to_owned(),
    );
    let owner_source = "FROM python:3.14-trixie AS base\n\
                        COPY --from=ghcr.io/astral-sh/uv:latest /uv /uvx /bin/\n\
                        RUN /bin/uv --version\n";
    let references = external_references(owner_source, None).expect("the owner Dockerfile parses");
    let mut owner_externals = BTreeMap::new();
    for reference in &references {
        let parsed =
            ImageReference::parse(&reference.reference).expect("the external reference parses");
        let pulled = https
            .pull_into(&parsed, &mut StepLog::discard())
            .unwrap_or_else(|error| {
                panic!(
                    "fail closed: pull `{}` from ghcr.io failed: {error}\nnext: check network egress to ghcr.io and retry",
                    reference.reference
                )
            });
        owner_externals.insert(
            reference.reference.clone(),
            ExternalImage {
                rootfs: pulled.rootfs,
                digest: pulled.digest,
            },
        );
    }
    // The `FROM python:3.14-trixie` base is a public HTTPS registry pull, so it needs the
    // default-scheme puller: the loopback `builder` above speaks plain HTTP, and its bearer token
    // is dropped by the registry's 301 → HTTPS redirect, failing the base pull as `Unauthorized`.
    let owner_builder = NativeImageBuilder::new(
        Arc::clone(&engine) as Arc<dyn WorldEngine>,
        https,
        store.clone(),
        arch,
        StepHost::new(&crun, "/etc/resolv.conf", &scratch, uid, gid),
    );
    fs::write(&dockerfile, owner_source).expect("write the owner Dockerfile");
    let owner_request = BuildRequest {
        context: context.clone(),
        dockerfile: dockerfile.clone(),
        args: BuildArgs::new(),
        target: None,
        externals: owner_externals,
    };
    let (owner, owner_log) = build_capture(&owner_builder, &owner_request);
    let owner = owner.unwrap_or_else(|error| {
        panic!(
            "owner Dockerfile build: {error}\nbuild output tail:\n{}",
            output_tail(&owner_log)
        )
    });
    assert!(
        owner.rootfs.join("bin/uv").is_file(),
        "the external `uv` binary must land in the built rootfs"
    );
    let uv_version = owner_log
        .iter()
        .filter(|(stream, _)| *stream == Stream::Stdout)
        .map(|(_, line)| line.trim_start())
        .any(|line| {
            line.starts_with("uv ")
                && line.split_whitespace().nth(1).is_some_and(|version| {
                    version.chars().next().is_some_and(|c| c.is_ascii_digit())
                })
        });
    assert!(
        uv_version,
        "`uv --version` must print `uv <digit>…`, saw:\n{}",
        output_tail(&owner_log)
    );

    drop(fixture);
    destroy_children(&engine, &store.snapshots_root());
    destroy_children(&engine, &store.images_root().join("oci"));
    let _ = fs::remove_dir_all(&run);
}

/// A real Debian build on `python:3.9-slim` (Debian trixie): the Python 3.9 image is end-of-life,
/// so its tag is frozen at Oct 2025 and ships baked `/var/lib/apt/lists/*InRelease` whose
/// `Valid-Until` is long past — the deterministic owner case. An unpack that stamped `now` makes
/// `apt` trust the expired list; the restored image-build mtime makes it re-fetch. `RUN` is
/// container root (so `apt-get` installs system packages), `USER app`/`USER postgres` run as the
/// accounts the image itself defines, and the image's own `/etc/passwd` is never replaced by a
/// synthetic host file.
#[test]
fn debian_root_build_installs_system_packages_and_switches_named_users() {
    let crun = crun_path();
    // SAFETY: `getuid`/`getgid` only read the caller's identity and cannot fail.
    let uid = unsafe { libc::getuid() };
    // SAFETY: as above; `getgid` has no preconditions.
    let gid = unsafe { libc::getgid() };
    let run = support::live_run_root("dockerfile-debian");
    let store = StoreLayout::new(&run);
    let engine = Arc::new(BtrfsEngine::new(store.clone()));
    engine.ensure_store().expect("create the Btrfs image store");
    let scratch = run.join("bundles");
    let arch = support::host_architecture();
    let https = OciPuller::new(
        Arc::clone(&engine) as Arc<dyn WorldEngine>,
        ImageLayout::new(store.images_root()),
        RegistryCredentials::default(),
        arch,
        "slipfork-tests/0.1".to_owned(),
    );
    // M3.143: the base carries `/var/lib/apt/lists/*InRelease` stamped at its own build date. An
    // unpack that wrote `now` made `apt` send a future `If-Modified-Since`, the mirror answered
    // `304`, and the image-era `Valid-Until` (long expired) aborted `apt-get update`. Stream the
    // cold pull: this base is 50+ MB and a silent fetch looks hung.
    let base = {
        let mut events = |event: BuildEvent<'_>| match event {
            BuildEvent::Line { text, .. } | BuildEvent::Note { text } => println!("{text}"),
            BuildEvent::Step(_) => {}
        };
        let mut log = StepLog::with_events(&mut events);
        https
            .pull_row(
                &ImageReference::parse("python:3.9-slim").expect("base reference"),
                "python:3.9-slim",
                &mut log,
            )
            .expect("pull the base once to inspect its baked apt lists")
    };
    // The frozen tag ships three fixed `*InRelease` header mtimes; the security list is one of them.
    const BAKED_SECURITY_INSTANT: u64 = 1_761_923_001;
    let lists = base.rootfs.join("var/lib/apt/lists");
    let in_release = fs::read_dir(&lists)
        .expect("the base carries baked apt lists")
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .find(|path| {
            path.file_name().is_some_and(|name| {
                name.to_string_lossy()
                    .ends_with("trixie-security_InRelease")
            })
        })
        .expect("the base ships the baked `trixie-security` InRelease");
    let baked = fs::metadata(&in_release)
        .expect("stat the baked InRelease")
        .modified()
        .expect("read the baked mtime");
    let build_start = SystemTime::now();
    assert_eq!(
        baked,
        UNIX_EPOCH + Duration::from_secs(BAKED_SECURITY_INSTANT),
        "`{}` must keep the frozen layer header mtime, not `now`",
        in_release.display()
    );
    println!(
        "M3.143 evidence: {} mtime {:?} ({} days old)",
        in_release.display(),
        baked,
        build_start
            .duration_since(baked)
            .expect("the baked lists predate this build")
            .as_secs()
            / 86_400
    );
    let builder = NativeImageBuilder::new(
        Arc::clone(&engine) as Arc<dyn WorldEngine>,
        https,
        store.clone(),
        arch,
        StepHost::new(crun, "/etc/resolv.conf", &scratch, uid, gid),
    );
    let context = run.join("context");
    fs::create_dir_all(&context).expect("create the build context");
    let dockerfile = context.join("Dockerfile");
    fs::write(
        &dockerfile,
        "FROM python:3.9-slim\n\
         RUN whoami\n\
         RUN grep -q 'Sandbox::User \"root\"' \"$APT_CONFIG\"\n\
         RUN apt-get update && apt-get install -y --no-install-recommends curl postgresql\n\
         RUN useradd --create-home --uid 1001 app\n\
         USER app\n\
         RUN id\n\
         USER postgres\n\
         RUN d=$(ls -d /usr/lib/postgresql/*/bin | head -1) && \"$d/initdb\" -D /var/lib/postgresql/data --no-locale -A trust\n",
    )
    .expect("write the Debian Dockerfile");
    let context_before = snapshot_tree(&context);
    let request = BuildRequest {
        context: context.clone(),
        dockerfile: dockerfile.clone(),
        args: BuildArgs::new(),
        target: None,
        externals: BTreeMap::new(),
    };
    let (report, log) = build_capture(&builder, &request);
    let report = report.unwrap_or_else(|error| {
        panic!(
            "Debian root build: {error}\nbuild output tail:\n{}",
            output_tail(&log)
        )
    });
    let apt_lines: Vec<String> = log
        .iter()
        .filter(|(stream, _)| *stream == Stream::Stdout)
        .map(|(_, line)| clean_line(line.trim_start()))
        .collect();
    assert!(
        apt_lines
            .iter()
            .any(|line| line.starts_with("Get:") && line.contains("InRelease")),
        "`apt-get update` must `Get:` the stale `InRelease` instead of trusting a `now` mtime, saw:\n{}",
        output_tail(&log)
    );
    assert!(
        !apt_lines
            .iter()
            .any(|line| line.starts_with("Hit:") && line.contains("InRelease")),
        "a `Hit:`/`304` for `InRelease` means the unpack still stamped `now`, saw:\n{}",
        output_tail(&log)
    );
    assert_eq!(
        snapshot_tree(&context),
        context_before,
        "the checkout is untouched: a build must never rewrite its context"
    );
    assert!(
        log.iter()
            .any(|(stream, line)| *stream == Stream::Stdout && line.trim() == "root"),
        "`RUN whoami` must stream `root`, saw:\n{}",
        output_tail(&log)
    );
    assert!(
        log.iter()
            .any(|(stream, line)| *stream == Stream::Stdout && line.contains("uid=1001(app)")),
        "`USER app` must run `id` as uid 1001(app), saw:\n{}",
        output_tail(&log)
    );
    let curl = report.rootfs.join("usr/bin/curl");
    assert!(
        curl.is_file(),
        "`apt-get install curl` must land `/usr/bin/curl`, saw:\n{}",
        output_tail(&log)
    );
    assert_eq!(
        fs::metadata(&curl).expect("stat curl").uid(),
        uid,
        "a file installed by the root step is owned by the invoking uid on disk"
    );
    let pg_version = report.rootfs.join("var/lib/postgresql/data/PG_VERSION");
    assert!(
        pg_version.is_file(),
        "`USER postgres` + initdb must write PG_VERSION, saw:\n{}",
        output_tail(&log)
    );
    assert_eq!(
        fs::metadata(&pg_version).expect("stat PG_VERSION").uid(),
        uid,
        "initdb output is host-uid-owned even though it ran as postgres inside"
    );
    assert!(
        fs::metadata(&pg_version)
            .expect("stat PG_VERSION")
            .modified()
            .expect("read the PG_VERSION mtime")
            >= build_start,
        "a `RUN`-written file keeps `now`, never a layer mtime"
    );
    assert_eq!(
        fs::metadata(report.rootfs.join("home/app"))
            .expect("stat /home/app")
            .uid(),
        uid,
        "`useradd --create-home` runs as container root, so the home is host-owned"
    );
    let passwd =
        fs::read_to_string(report.rootfs.join("etc/passwd")).expect("read the image passwd");
    assert!(
        passwd.contains("app:x:1001"),
        "the image's own `/etc/passwd` must keep the account that `useradd` wrote"
    );
    // Fail-closed leg on the same store boot: `USER ghost` is metadata only, so the next `RUN`
    // resolves the account from the image's own `/etc/passwd` and must fail at line 3.
    let ghost = context.join("Ghostfile");
    fs::write(&ghost, "FROM python:3.9-slim\nUSER ghost\nRUN id\n")
        .expect("write the unknown-account Dockerfile");
    let ghost_request = BuildRequest {
        dockerfile: ghost,
        ..request.clone()
    };
    let (error, ghost_log) = build_capture(&builder, &ghost_request);
    let error = error.expect_err("`USER ghost` must fail closed on an account the image lacks");
    let rendered = error.to_string();
    assert!(
        matches!(error, slipfork_image::Error::StepFailed { line: 3, .. }),
        "the failure decorates the `RUN id` line: {rendered}\nbuild output tail:\n{}",
        output_tail(&ghost_log)
    );
    assert!(
        rendered.contains("ghost"),
        "the account is named: {rendered}"
    );
    assert!(
        error.remedy().is_some_and(|next| next.contains("useradd")),
        "the remedy points at useradd: {rendered}"
    );

    destroy_children(&engine, &store.snapshots_root());
    destroy_children(&engine, &store.images_root().join("oci"));
    let _ = fs::remove_dir_all(&run);
}

/// Real-Btrfs key isolation for `COPY`: an edit to a file no `COPY` names stays sealed, an edited
/// copied file rebuilds only its step and suffix (both rebuilt rootfs bytes are read back), and
/// `Cache::Cold` re-executes every step even though the keys are unchanged.
#[test]
fn copy_keys_isolate_edits_and_cold_rebuilds_every_step() {
    let run = support::live_run_root("dockerfile-copy-keys");
    // SAFETY: `getuid`/`getgid` only read the caller's identity and cannot fail.
    let uid = unsafe { libc::getuid() };
    // SAFETY: as above; `getgid` has no preconditions.
    let gid = unsafe { libc::getgid() };
    let store = StoreLayout::new(&run);
    let engine = Arc::new(BtrfsEngine::new(store.clone()));
    engine.ensure_store().expect("create the Btrfs image store");
    let scratch = run.join("bundles");
    let fixture = support::Fixture::start();
    let arch = support::host_architecture();
    let puller = OciPuller::new(
        Arc::clone(&engine) as Arc<dyn WorldEngine>,
        ImageLayout::new(store.images_root()),
        RegistryCredentials::default(),
        arch,
        "slipfork-tests/0.1".to_owned(),
    )
    .with_scheme(RegistryScheme::Http);
    let builder = NativeImageBuilder::new(
        Arc::clone(&engine) as Arc<dyn WorldEngine>,
        puller,
        store.clone(),
        arch,
        StepHost::new(crun_path(), "/etc/resolv.conf", &scratch, uid, gid),
    );

    let context = run.join("context");
    fs::create_dir_all(context.join("app")).expect("create the copied app dir");
    fs::create_dir_all(context.join("docs")).expect("create the copied docs dir");
    fs::write(context.join("app/copied.txt"), "one\n").expect("write the first copy source");
    fs::write(context.join("docs/copied.txt"), "docs-one\n").expect("write the middle source");
    fs::write(context.join("tail.txt"), "tail\n").expect("write the suffix source");
    fs::write(context.join("notes.md"), "untracked\n").expect("write an uncopied file");
    let dockerfile = context.join("Dockerfile");
    fs::write(
        &dockerfile,
        "FROM scratch\nCOPY app /app\nCOPY docs /docs\nCOPY tail.txt /tail.txt\n",
    )
    .expect("write the three-COPY Dockerfile");
    let request = BuildRequest {
        context: context.clone(),
        dockerfile: dockerfile.clone(),
        args: BuildArgs::new(),
        target: None,
        externals: BTreeMap::new(),
    };

    let (first, first_log) = build_capture(&builder, &request);
    let first = first.unwrap_or_else(|error| {
        panic!(
            "first COPY-only build: {error}\nbuild output tail:\n{}",
            output_tail(&first_log)
        )
    });
    assert_eq!(first.cache_misses, 3, "three COPY steps graft from scratch");
    assert_eq!(
        support::read_text(&first.rootfs.join("app/copied.txt")),
        "one\n",
        "the first COPY carries its source bytes"
    );
    assert_eq!(
        support::read_text(&first.rootfs.join("docs/copied.txt")),
        "docs-one\n",
        "the middle COPY carries its source bytes"
    );
    assert_eq!(
        support::read_text(&first.rootfs.join("tail.txt")),
        "tail\n",
        "the suffix COPY carries its source bytes"
    );

    // A context edit no `COPY` names never moves a source key.
    fs::write(context.join("notes.md"), "edited but uncopied\n").expect("edit the uncopied file");
    let (unrelated, unrelated_log) = build_capture(&builder, &request);
    let unrelated = unrelated.unwrap_or_else(|error| {
        panic!(
            "unrelated-context rebuild: {error}\nbuild output tail:\n{}",
            output_tail(&unrelated_log)
        )
    });
    assert_eq!(
        unrelated.cache_misses, 0,
        "an uncopied context edit must stay sealed"
    );
    assert_eq!(unrelated.cache_hits, 3);

    // Editing the middle `COPY`'d file rebuilds that step and its suffix only.
    fs::write(context.join("docs/copied.txt"), "docs-two\n").expect("edit the middle source");
    let (edited, edited_log) = build_capture(&builder, &request);
    let edited = edited.unwrap_or_else(|error| {
        panic!(
            "edited-COPY rebuild: {error}\nbuild output tail:\n{}",
            output_tail(&edited_log)
        )
    });
    assert_eq!(
        edited.cache_misses, 2,
        "the edited `COPY docs` step and its `COPY tail.txt` suffix rebuild"
    );
    assert_eq!(edited.cache_hits, 1, "`COPY app` stays sealed");
    assert_eq!(
        support::read_text(&edited.rootfs.join("docs/copied.txt")),
        "docs-two\n",
        "the rebuilt middle COPY must carry the edited bytes"
    );
    assert_eq!(
        support::read_text(&edited.rootfs.join("tail.txt")),
        "tail\n",
        "the rebuilt suffix keeps its source bytes intact"
    );
    assert_eq!(
        support::read_text(&edited.rootfs.join("app/copied.txt")),
        "one\n",
        "the earlier sealed COPY keeps its bytes"
    );

    // `Cache::Cold` re-resolves and re-executes every materialized node, keys unchanged.
    let cold = builder.with_cache(Cache::Cold);
    let (forced, forced_log) = build_capture(&cold, &request);
    let forced = forced.unwrap_or_else(|error| {
        panic!(
            "`Cache::Cold` rebuild: {error}\nbuild output tail:\n{}",
            output_tail(&forced_log)
        )
    });
    assert_eq!(
        forced.cache_misses, 3,
        "`--no-cache` must re-execute every COPY step"
    );
    assert_eq!(forced.cache_hits, 0);

    drop(fixture);
    destroy_children(&engine, &store.snapshots_root());
    destroy_children(&engine, &store.images_root().join("oci"));
    let _ = fs::remove_dir_all(&run);
}

fn dockerfile_source(base: &str, url: &str, marker: &str, uid: u32, gid: u32) -> String {
    format!(
        "FROM {base} AS builder\n\
         RUN echo built > /root/out.txt\n\
         FROM {base} AS final\n\
         COPY --from=builder /root/out.txt /app/from-builder.txt\n\
         COPY --chown={uid}:{gid} owned.txt /owned.txt\n\
         COPY . /app\n\
         ADD {url} /opt/data/\n\
         WORKDIR /app\n\
         ENV APP_MODE=production\n\
         RUN --mount=type=cache,target=/root/.cache/x <<EOF\n\
         echo {marker} >> /root/.cache/x/out.txt\n\
         echo built > /root/out.txt\n\
         EOF\n\
         USER {uid}:{gid}\n\
         EXPOSE 8080 9090/udp\n\
         CMD [\"/app/run\", \"--serve\"]\n"
    )
}

/// Recursive `(relative path, len, mode & 0o7777, mtime)` snapshot of a build context, sorted so a
/// build that rewrites, chmods, or touches its own checkout is caught byte-for-byte.
fn snapshot_tree(root: &Path) -> Vec<(String, u64, u32, i64)> {
    fn walk(root: &Path, relative: &Path, out: &mut Vec<(String, u64, u32, i64)>) {
        let directory = root.join(relative);
        let Ok(entries) = fs::read_dir(&directory) else {
            return;
        };
        for entry in entries.flatten() {
            let next = relative.join(entry.file_name());
            let path = root.join(&next);
            let metadata = fs::symlink_metadata(&path).expect("stat a context entry");
            out.push((
                next.to_string_lossy().into_owned(),
                metadata.len(),
                metadata.mode() & 0o7777,
                metadata.mtime(),
            ));
            if metadata.is_dir() {
                walk(root, &next, out);
            }
        }
    }
    let mut out = Vec::new();
    walk(root, Path::new(""), &mut out);
    out.sort_by(|left, right| left.0.cmp(&right.0));
    out
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
/// Whether `path` is one of the store's `<64-hex>` node slots, never a record, lock or temp file.
fn is_node_dir(path: &Path) -> bool {
    path.is_dir()
        && path
            .file_name()
            .and_then(|name| name.to_str())
            .is_some_and(|name| {
                name.len() == 64 && name.bytes().all(|byte| byte.is_ascii_hexdigit())
            })
}

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
