//! Protected live test: a real `docker buildx` extraction of the `slipfork-world` stage.
//!
//! Runs a 3-line fixture `Dockerfile` with a named stage, a real pinned static `crun`, and the real
//! `slipfork-world-init` binary, then proves a second materialization is a zero-Docker cache hit.
//! When `docker`/`buildx` is absent the test **fails closed** with the actionable diagnostic — it
//! never skips silently.

mod support;

use std::path::{Path, PathBuf};
use std::sync::Arc;

use slipfork_engine::rootfs::{
    BackingServiceKey, BaseImageBuilder, BaseImageError, CliBuildxExporter, DockerfileStageBuilder,
    ElfAbiProbe, FsInputHasher, HostCacheRequest, LibcFamily, OciImageStore, OciRootfs,
    RootfsBuildEvent, RuntimeAssets, SnowPortContract, WorldRootfsRequest,
};

const DOCKERFILE: &str =
    "FROM alpine:3.21 AS app\nWORKDIR /workspace\nRUN echo slipfork > /etc/slipfork-probe\n";

struct NoServices;

impl OciImageStore for NoServices {
    fn materialize(&self, image: &BackingServiceKey) -> Result<OciRootfs, BaseImageError> {
        panic!("the live fixture requests no backing services, got {image}");
    }
}

struct LiveHarness {
    _dir: tempfile::TempDir,
    request: WorldRootfsRequest,
}

impl LiveHarness {
    fn new() -> Self {
        let dir = support::disk_tempdir("rootfs-live-");
        let context = dir.path().join("context");
        std::fs::create_dir_all(&context).expect("context");
        std::fs::write(context.join("Dockerfile"), DOCKERFILE).expect("Dockerfile");
        let world_init = PathBuf::from(env!("CARGO_BIN_EXE_slipfork-world-init"));
        assert!(
            world_init.is_file(),
            "cargo must build the slipfork-world-init helper for the live test: {}",
            world_init.display()
        );
        let request = WorldRootfsRequest {
            state_dir: dir.path().join("state"),
            image_id: "app".to_owned(),
            context_dir: context.clone(),
            context_label: ".".to_owned(),
            dockerfile: context.join("Dockerfile"),
            target_stage: None,
            build_envs: Vec::new(),
            target_arch: "x86_64".to_owned(),
            services: Vec::new(),
            ports: SnowPortContract {
                component: "app".to_owned(),
                internal_ports: Vec::new(),
                ingress_ports: Vec::new(),
            },
            uid: 1000,
            gid: 1000,
            runtime: RuntimeAssets {
                crun: support::pinned_crun(),
                world_init,
            },
            host_caches: HostCacheRequest::default(),
        };
        Self { _dir: dir, request }
    }

    fn builder(&self, docker_program: &str) -> DockerfileStageBuilder {
        DockerfileStageBuilder::new(
            Arc::new(CliBuildxExporter::new(docker_program)),
            Arc::new(FsInputHasher),
            Arc::new(NoServices),
            Arc::new(ElfAbiProbe),
        )
    }
}

#[test]
#[ignore = "requires docker buildx and the pinned static crun (real local export)"]
fn live_buildx_export_validates_and_caches_the_world_stage() {
    let harness = LiveHarness::new();
    let builder = harness.builder("docker");

    let mut events = Vec::new();
    let rootfs = builder
        .materialize(&harness.request, &mut |event| events.push(event))
        .unwrap_or_else(|error| panic!("live buildx extraction failed:\n{error}"));

    assert!(
        events
            .iter()
            .any(|event| matches!(event, RootfsBuildEvent::CacheMiss { .. })),
        "first materialization must be a cache miss: {events:?}"
    );
    assert!(
        events
            .iter()
            .any(|event| matches!(event, RootfsBuildEvent::Installed { .. })),
        "materialization must emit Installed: {events:?}"
    );
    assert_eq!(rootfs.digest.hex().len(), 64);
    assert_eq!(
        rootfs.app_rootfs,
        harness
            .request
            .state_dir
            .join("cache/rootfs")
            .join(rootfs.digest.hex())
            .join("app")
    );
    assert_eq!(rootfs.crun_bin, Path::new("/.slipfork/bin/crun"));

    for guest in ["/.slipfork/bin/crun", "/.slipfork/bin/slipfork-world-init"] {
        let path = rootfs.app_rootfs.join(guest.trim_start_matches('/'));
        assert!(path.is_file(), "extracted stage must contain {guest}");
        use std::os::unix::fs::PermissionsExt;
        assert!(
            std::fs::metadata(&path)
                .expect("metadata")
                .permissions()
                .mode()
                & 0o111
                != 0,
            "{guest} must be executable"
        );
    }
    assert!(
        rootfs.app_rootfs.join("etc/slipfork-probe").is_file(),
        "the user stage must be exported verbatim"
    );
    assert_eq!(rootfs.metadata.os_release.id, "alpine");
    assert!(
        rootfs.metadata.os_release.version_id.starts_with("3.21"),
        "alpine:3.21.x must match the supported 3.21 release, got {}",
        rootfs.metadata.os_release.version_id
    );
    assert_eq!(rootfs.metadata.elf_abi.libc, LibcFamily::Musl);
    assert_eq!(rootfs.metadata.elf_abi.arch, "x86_64");
    assert!(
        rootfs.service_images.is_empty(),
        "no backing services were requested"
    );

    // A second builder points at a nonexistent `docker`: a cache hit must perform zero Docker
    // work, proving the offline/fork guarantee (`ADR-0009` §6.2 rule 4).
    let offline = harness.builder("/nonexistent/slipfork-test-docker");
    let mut hit_events = Vec::new();
    let cached = offline
        .materialize(&harness.request, &mut |event| hit_events.push(event))
        .expect("cache hit must not need docker");
    assert!(
        hit_events
            .iter()
            .all(|event| !matches!(event, RootfsBuildEvent::CacheMiss { .. })),
        "second materialization must be a cache hit: {hit_events:?}"
    );
    assert_eq!(cached.digest, rootfs.digest);
    assert_eq!(cached.app_rootfs, rootfs.app_rootfs);
    assert_eq!(cached.metadata, rootfs.metadata);
}

#[test]
#[ignore = "requires docker buildx and the pinned static crun (real local export)"]
fn missing_docker_fails_closed_with_the_actionable_diagnostic() {
    let harness = LiveHarness::new();
    let builder = harness.builder("/nonexistent/slipfork-test-docker");
    let mut events = Vec::new();
    let error = builder
        .materialize(&harness.request, &mut |event| events.push(event))
        .expect_err("a missing docker must fail closed");
    assert!(matches!(error, BaseImageError::Buildx { .. }));
    let message = error.to_string();
    assert!(
        message.contains("install Docker with the buildx plugin"),
        "diagnostic must say how to fix it: {message}"
    );
    assert!(
        message.contains("docker buildx version"),
        "diagnostic must name the verification command: {message}"
    );
}
