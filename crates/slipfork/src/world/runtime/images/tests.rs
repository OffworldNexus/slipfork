//! Composition-root tests for [`ImageCache`]: a failed build and an unreachable external
//! `COPY --from` pull, kept beside the owner so `images.rs` stays under 500 lines.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use slipfork_forkfile::parse;
use slipfork_image::{Architecture, BuildReport, ImageConfig, RegistryCredentials, StepHost};
use slipfork_world::{BootState, BtrfsEngine, ImageNode, StoreLayout, WorldEngine, WorldName};

use super::{ImageCache, image_state};
use crate::world::supervise::{BootSink, Emitter};

/// A build that applied no filesystem step is `cached`, never a `<1ms` "built": this covers both
/// a fully reused image and a metadata-only `FROM`+`ENV` `Dockerfile`.
#[test]
fn zero_executed_steps_is_cached_not_built() {
    let report = BuildReport {
        rootfs: PathBuf::from("/images/web"),
        key: "deadbeef".to_owned(),
        config: ImageConfig::default(),
        cache_hits: 0,
        cache_misses: 0,
        build_time: Duration::from_micros(40),
        last: None,
        base_digest: None,
    };
    assert!(
        matches!(image_state(&report), BootState::Cached),
        "{report:?}"
    );
    // A partial reuse still applied a step, so it is a real build.
    let partially_reused = BuildReport {
        cache_hits: 2,
        cache_misses: 1,
        ..report
    };
    assert!(
        matches!(image_state(&partially_reused), BootState::Built),
        "{partially_reused:?}"
    );
}

/// A build the engine cannot even spawn has no child output: its raw one-line cause becomes the
/// capture, so `logs -r` never replays an empty block.
#[test]
fn a_failed_build_writes_no_synthetic_capture() {
    let root = tempfile::Builder::new()
        .prefix("image-cache-log")
        .tempdir_in("/var/tmp")
        .expect("disk-backed scratch root");
    let project = root.path().join("project");
    std::fs::create_dir_all(&project).expect("create the build context");
    std::fs::write(project.join("Dockerfile"), "FROM scratch\nRUN false\n")
        .expect("write the Dockerfile");
    let store = StoreLayout::new(root.path().join("store"));
    let engine = Arc::new(BtrfsEngine::new(store.clone()));
    let world = WorldName::parse("demo").expect("valid world name");
    let doc = parse("# forkfile: 0.1\n\nimage web . {\n  dockerfile Dockerfile\n}\n")
        .expect("the Forkfile parses");
    let cache = ImageCache::new(
        Arc::clone(&engine) as Arc<dyn WorldEngine>,
        &store,
        world.clone(),
        &project,
        &doc,
        vec![ImageNode {
            id: "web".to_owned(),
            directory: PathBuf::from("."),
            dockerfile: None,
            from: None,
            line: 3,
        }],
        BTreeMap::new(),
        Architecture::X86_64,
        RegistryCredentials::default(),
        "slipfork-tests/0.1",
        StepHost::new(
            "/nonexistent/crun",
            "/etc/resolv.conf",
            root.path().join("bundles"),
            1000,
            1000,
        ),
        "http",
        doc.default_features(),
        false,
    );
    let mut sink = BootSink::for_step(
        Arc::new(Mutex::new(Emitter::closed())),
        1,
        1,
        "image",
        "web",
    );
    let failure = cache
        .build("web", &mut sink)
        .expect_err("a missing crun must fail the image build");
    assert!(
        failure.message.contains("build `web` failed"),
        "{failure:?}"
    );
    // `crun` fails before step 1, so no step row and no capture exist: the verdict carries it.
    assert!(
        failure
            .note
            .reason
            .as_deref()
            .is_some_and(|reason| reason.contains("`RUN` needs `crun`")),
        "{failure:?}"
    );
    assert!(
        !store.pod_log(&world, "web").exists(),
        "a step-less failure writes no capture; its verdict carries the reason"
    );
}

/// The composition root pulls externals before the builder runs, so an unreachable
/// `COPY --from=<image>` fails at its own line and never seals an OCI rootfs.
#[test]
fn an_unpulled_external_copy_fails_at_its_dockerfile_line() {
    let root = tempfile::Builder::new()
        .prefix("image-cache-external")
        .tempdir_in("/var/tmp")
        .expect("disk-backed scratch root");
    let project = root.path().join("project");
    std::fs::create_dir_all(&project).expect("create the build context");
    // Bind then drop an ephemeral loopback port so the refusal is guaranteed, not assumed.
    let listener = std::net::TcpListener::bind("127.0.0.1:0")
        .expect("bind a loopback port to prove it is closed");
    let port = listener.local_addr().expect("read the probed port").port();
    drop(listener);
    let reference = format!("127.0.0.1:{port}/asset:latest");
    std::fs::write(
        project.join("Dockerfile"),
        format!("FROM 127.0.0.1:{port}/base:latest\nCOPY --from={reference} /x /y\n"),
    )
    .expect("write the Dockerfile");
    let store = StoreLayout::new(root.path().join("store"));
    let engine = Arc::new(BtrfsEngine::new(store.clone()));
    let world = WorldName::parse("demo").expect("valid world name");
    let doc = parse("# forkfile: 0.1\n\nimage web . {\n  dockerfile Dockerfile\n}\n")
        .expect("the Forkfile parses");
    let cache = ImageCache::new(
        Arc::clone(&engine) as Arc<dyn WorldEngine>,
        &store,
        world.clone(),
        &project,
        &doc,
        vec![ImageNode {
            id: "web".to_owned(),
            directory: PathBuf::from("."),
            dockerfile: None,
            from: None,
            line: 3,
        }],
        BTreeMap::new(),
        Architecture::X86_64,
        RegistryCredentials::default(),
        "slipfork-tests/0.1",
        StepHost::new(
            "/nonexistent/crun",
            "/etc/resolv.conf",
            root.path().join("bundles"),
            1000,
            1000,
        ),
        "http",
        doc.default_features(),
        false,
    );
    let mut sink = BootSink::for_step(
        Arc::new(Mutex::new(Emitter::closed())),
        1,
        1,
        "image",
        "web",
    );
    let failure = cache
        .build("web", &mut sink)
        .expect_err("the external pull must fail closed");
    assert!(
        failure
            .message
            .contains(&format!("pull `{reference}` failed")),
        "{failure:?}"
    );
    assert_eq!(failure.note.phase.as_deref(), Some("copy"));
    let instruction = format!("--from={reference}");
    assert_eq!(
        failure.note.instruction.as_deref(),
        Some(instruction.as_str())
    );
    assert_eq!(failure.note.source.as_deref(), Some("Dockerfile:2"));
    assert!(
        failure
            .note
            .reason
            .as_deref()
            .is_some_and(|reason| reason.contains("could not reach")),
        "{failure:?}"
    );
    assert!(
        !store.images_root().join("oci").exists(),
        "no OCI rootfs may be sealed for an unreachable external reference"
    );
}
