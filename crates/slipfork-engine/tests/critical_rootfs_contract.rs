//! Protected critical tests for the `ADR-0009` 6-rule World Image Contract.
//!
//! Every rule asserts its typed [`BaseImageError`] variant **and** the exact user-facing diagnostic
//! (contract prefix + `  fix:` line). Rules 1/3/4/6 are static; rules 2/5 run against a real
//! extracted directory tree produced by an injected exporter — never a mocked validator. No Docker
//! daemon is touched here.

pub mod support;

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use slipfork_engine::MountMode;
use slipfork_engine::rootfs::{
    BackingServiceKey, BaseImageBuilder, BaseImageError, BuildxExporter, BuildxRequest,
    DockerfileStageBuilder, ElfAbiProbe, HostCacheRequest, ImageContractMetadata, InputHasher,
    OciImageStore, OciRootfs, OsRelease, RootfsBuildEvent, RuntimeAssets, SnowPortContract,
    WorldRootfs, WorldRootfsRequest, conformance::assert_world_rootfs_conformance,
};
use slipfork_engine::rootfs::{CRUN_GUEST_PATH, WORLD_INIT_GUEST_PATH};
use tempfile::TempDir;

const ALPINE_OS_RELEASE: &str = "ID=alpine\nVERSION_ID=3.21\n";

/// Exporter fake: copies a prepared fixture tree into the local-export destination.
struct FixtureExporter {
    template: PathBuf,
    calls: Arc<AtomicUsize>,
}

impl BuildxExporter for FixtureExporter {
    fn build_local(&self, request: &BuildxRequest<'_>) -> Result<(), BaseImageError> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        support::copy_tree(&self.template, request.dest_dir).map_err(|error| BaseImageError::Io {
            path: request.dest_dir.display().to_string(),
            message: error.to_string(),
        })
    }
}

/// Deterministic context hash (the Dockerfile bytes still feed the digest directly).
struct FixedHasher;

impl InputHasher for FixedHasher {
    fn hash_context(&self, _context: &Path) -> Result<String, BaseImageError> {
        Ok("a".repeat(64))
    }
}

/// No service images are requested by these contract fixtures.
struct NoOciStore;

impl OciImageStore for NoOciStore {
    fn materialize(&self, image: &BackingServiceKey) -> Result<OciRootfs, BaseImageError> {
        panic!("contract fixtures request no backing services, got {image}");
    }
}

fn builder(template: &Path) -> (DockerfileStageBuilder, Arc<AtomicUsize>) {
    let calls = Arc::new(AtomicUsize::new(0));
    let builder = DockerfileStageBuilder::new(
        Arc::new(FixtureExporter {
            template: template.to_path_buf(),
            calls: calls.clone(),
        }),
        Arc::new(FixedHasher),
        Arc::new(NoOciStore),
        Arc::new(ElfAbiProbe),
    );
    (builder, calls)
}

struct Harness {
    dir: TempDir,
    dockerfile: PathBuf,
    context: PathBuf,
    state_dir: PathBuf,
    runtime: RuntimeAssets,
}

impl Harness {
    fn new(dockerfile: &str) -> Self {
        let dir = support::disk_tempdir("contract-");
        let root = dir.path().to_path_buf();
        let context = root.join("context");
        std::fs::create_dir_all(&context).expect("context");
        let dockerfile_path = context.join("Dockerfile");
        std::fs::write(&dockerfile_path, dockerfile).expect("write Dockerfile");
        let crun = root.join("crun.bin");
        std::fs::write(&crun, b"fixture crun").expect("write crun");
        let world_init = root.join("slipfork-world-init");
        std::fs::write(&world_init, b"fixture world init").expect("write world init");
        Self {
            dir,
            dockerfile: dockerfile_path,
            context,
            state_dir: root.join("state"),
            runtime: RuntimeAssets { crun, world_init },
        }
    }

    fn root(&self) -> &Path {
        self.dir.path()
    }

    fn request(&self, stage: Option<&str>) -> WorldRootfsRequest {
        WorldRootfsRequest {
            state_dir: self.state_dir.clone(),
            image_id: "api".to_owned(),
            context_dir: self.context.clone(),
            context_label: ".".to_owned(),
            dockerfile: self.dockerfile.clone(),
            target_stage: stage.map(str::to_owned),
            build_envs: Vec::new(),
            target_arch: "x86_64".to_owned(),
            services: Vec::new(),
            ports: SnowPortContract {
                component: "api".to_owned(),
                internal_ports: vec![8000],
                ingress_ports: vec![8000],
            },
            uid: 1000,
            gid: 1000,
            runtime: self.runtime.clone(),
            host_caches: HostCacheRequest::default(),
        }
    }
}

fn expect_error(builder: &DockerfileStageBuilder, request: &WorldRootfsRequest) -> BaseImageError {
    let mut events = |_event: RootfsBuildEvent| {};
    match builder.materialize(request, &mut events) {
        Err(error) => error,
        Ok(rootfs) => panic!("expected a fail-closed contract error, got {rootfs:?}"),
    }
}

fn expect_message(error: &BaseImageError, expected: &str) {
    assert_eq!(error.to_string(), expected, "got {error:?}");
}

/// Build a real extracted-tree fixture on disk.
fn fixture_tree(
    root: &Path,
    os_release: Option<&str>,
    with_shell: bool,
    extra_files: &[(&str, bool)],
) -> PathBuf {
    let tree = root.join("fixture-rootfs");
    std::fs::create_dir_all(tree.join("etc")).expect("etc");
    if let Some(os_release) = os_release {
        std::fs::write(tree.join("etc/os-release"), os_release).expect("os-release");
    }
    if with_shell {
        std::fs::create_dir_all(tree.join("bin")).expect("bin");
        std::fs::copy("/bin/sh", tree.join("bin/sh")).expect("copy host /bin/sh");
    }
    for (guest, executable) in extra_files {
        let path = tree.join(guest.trim_start_matches('/'));
        std::fs::create_dir_all(path.parent().expect("parent")).expect("fixture parent");
        std::fs::write(&path, b"fixture").expect("fixture file");
        if *executable {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755))
                .expect("fixture mode");
        }
    }
    tree
}

#[test]
fn rule_1_unnamed_final_stage_reports_the_exact_diagnostic() {
    let harness = Harness::new("FROM alpine:3.21\n");
    let (builder, _) = builder(harness.root());
    let error = expect_error(&builder, &harness.request(None));
    assert!(matches!(
        error,
        BaseImageError::UnnamedTargetStage { line: 1, .. }
    ));
    expect_message(
        &error,
        "error: World Image Contract #1 violated in Dockerfile:1: final stage `FROM alpine:3.21` has no stage name\n  fix: change Dockerfile:1 to `FROM alpine:3.21 AS app` (or set `world.stage = \"<stage>\"` in slipfork.toml)",
    );
}

#[test]
fn rule_1_unknown_stage_lists_the_declared_stages() {
    let harness = Harness::new("FROM alpine:3.21 AS builder\nFROM builder AS runtime\n");
    let (builder, _) = builder(harness.root());
    let error = expect_error(&builder, &harness.request(Some("dev")));
    assert!(matches!(error, BaseImageError::UnknownTargetStage { .. }));
    expect_message(
        &error,
        "error: World Image Contract #1 violated: `world.stage = \"dev\"` not found in Dockerfile (declared stages: 'builder', 'runtime')\n  fix: set `world.stage = \"builder\"` in slipfork.toml",
    );
}

#[test]
fn rule_2_unsupported_distro_names_the_supported_table() {
    let harness = Harness::new("FROM fedora:41 AS runtime\n");
    let tree = fixture_tree(
        harness.root(),
        Some("ID=fedora\nVERSION_ID=41\n"),
        false,
        &[],
    );
    let (builder, calls) = builder(&tree);
    let error = expect_error(&builder, &harness.request(None));
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    assert!(matches!(
        error,
        BaseImageError::UnsupportedDistro { line: 1, .. }
    ));
    expect_message(
        &error,
        "error: World Image Contract #2 violated in stage 'runtime' (Dockerfile:1): detected distro 'fedora:41'; supported distros are debian (bookworm, trixie), ubuntu (jammy, noble), and alpine (3.20, 3.21)\n  fix: set `world.stage = \"builder\"` in slipfork.toml or switch `FROM` to a supported Debian, Ubuntu, or Alpine base",
    );
}

#[test]
fn rule_2_missing_os_release_and_shell_fails_closed() {
    let harness = Harness::new("FROM scratch AS runtime\n");
    let tree = fixture_tree(harness.root(), None, false, &[]);
    let (builder, _) = builder(&tree);
    let error = expect_error(&builder, &harness.request(None));
    assert!(matches!(error, BaseImageError::UnsupportedDistro { .. }));
    expect_message(
        &error,
        "error: World Image Contract #2 violated in stage 'runtime' (Dockerfile:1): missing /etc/os-release + /bin/sh; supported distros are debian (bookworm, trixie), ubuntu (jammy, noble), and alpine (3.20, 3.21)\n  fix: set `world.stage = \"builder\"` in slipfork.toml or switch `FROM` to a supported Debian, Ubuntu, or Alpine base",
    );
}

#[test]
fn rule_3_root_only_entrypoint_reports_uid_gid_and_fix() {
    let harness = Harness::new(
        "FROM debian:trixie-slim AS api\nENTRYPOINT [\"docker-entrypoint.sh\", \"gosu\", \"appuser\"]\n",
    );
    let (builder, _) = builder(harness.root());
    let error = expect_error(&builder, &harness.request(None));
    assert!(matches!(
        error,
        BaseImageError::RootAtRuntimeEntrypoint {
            uid: 1000,
            gid: 1000,
            ..
        }
    ));
    expect_message(
        &error,
        "error: World Image Contract #3 violated in stage 'api': ENTRYPOINT [\"docker-entrypoint.sh\", \"gosu\", \"appuser\"] requires root to switch UIDs, but Slipfork always executes as host $UID:$GID (1000:1000) with a read-only '/' filesystem\n  fix: set an unprivileged command in snow.yml `modes.local.components.api.run_command` (or remove `gosu`/`su-exec`/`chown` from ENTRYPOINT)",
    );
}

#[test]
fn rule_4_baked_origin_env_fails_closed() {
    let harness =
        Harness::new("FROM debian:trixie-slim AS api\nENV BASE_URL=http://localhost:8000\n");
    let (builder, _) = builder(harness.root());
    let error = expect_error(&builder, &harness.request(None));
    assert!(matches!(
        error,
        BaseImageError::BakedOriginOrHostPath { .. }
    ));
    expect_message(
        &error,
        "error: World Image Contract #4 violated in stage 'api': baked image ENV `BASE_URL=http://localhost:8000` conflicts with Slipfork single-origin and ResourceCoordinates injection\n  fix: delete ENV `BASE_URL=http://localhost:8000` from Dockerfile and reference `${{ resources::api.url }}` under `modes.all.envs` in snow.yml",
    );
}

#[test]
fn rule_4_baked_host_workdir_fails_closed() {
    let harness = Harness::new("FROM debian:trixie-slim AS api\nWORKDIR /home/alice/app\n");
    let (builder, _) = builder(harness.root());
    let error = expect_error(&builder, &harness.request(None));
    assert!(matches!(
        error,
        BaseImageError::BakedOriginOrHostPath { .. }
    ));
    expect_message(
        &error,
        "error: World Image Contract #4 violated in stage 'api': baked image host path `/home/alice/app` conflicts with Slipfork single-origin and ResourceCoordinates injection\n  fix: delete host path `/home/alice/app` from Dockerfile and reference `${{ resources::api.url }}` under `modes.all.envs` in snow.yml",
    );
}

#[test]
fn rule_5_backing_service_inside_the_app_stage_fails_closed() {
    let harness = Harness::new("FROM alpine:3.21 AS api\n");
    let tree = fixture_tree(
        harness.root(),
        Some(ALPINE_OS_RELEASE),
        true,
        &[("/usr/lib/postgresql/18/bin/postgres", true)],
    );
    let (builder, calls) = builder(&tree);
    let error = expect_error(&builder, &harness.request(None));
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    assert!(matches!(
        error,
        BaseImageError::EmbeddedBackingService { .. }
    ));
    expect_message(
        &error,
        "error: World Image Contract #5 violated in stage 'api': detected `/usr/lib/postgresql/18/bin/postgres` (backing service daemon) installed inside the app image; Slipfork runs postgres, redis, and seaweedfs as isolated crun containers from snow.yml\n  fix: remove `postgresql` server installation from Dockerfile and declare `resources.pg_clusters` in snow.yml",
    );
}

#[test]
fn rule_6_expose_port_mismatch_fails_closed() {
    let harness = Harness::new("FROM debian:trixie-slim AS api\nEXPOSE 3000\n");
    let (builder, _) = builder(harness.root());
    let error = expect_error(&builder, &harness.request(None));
    assert!(matches!(
        error,
        BaseImageError::ExposeIngressMismatch { port: 3000, .. }
    ));
    expect_message(
        &error,
        "error: World Image Contract #6 violated for image 'api': Dockerfile EXPOSE declares port 3000, but snow.yml component 'api' declares internal_ports [8000] (ingresses target 8000)\n  fix: change `EXPOSE 3000` to `EXPOSE 8000` in Dockerfile (or update `components.api.internal_ports` / `ingresses` in snow.yml to 3000)",
    );
}

#[test]
fn rule_6_healthcheck_port_is_validated_too() {
    let harness = Harness::new(
        "FROM debian:trixie-slim AS api\nHEALTHCHECK --interval=30s CMD curl -fsS http://localhost:3000/healthz || exit 1\nEXPOSE 8000\n",
    );
    let (builder, _) = builder(harness.root());
    let error = expect_error(&builder, &harness.request(None));
    assert!(matches!(
        error,
        BaseImageError::ExposeIngressMismatch {
            instruction: "HEALTHCHECK",
            port: 3000,
            ..
        }
    ));
}

#[test]
fn runtime_binaries_missing_from_the_extracted_stage_fail_closed() {
    let harness = Harness::new("FROM alpine:3.21 AS api\n");
    let tree = fixture_tree(harness.root(), Some(ALPINE_OS_RELEASE), true, &[]);
    let (builder, _) = builder(&tree);
    let error = expect_error(&builder, &harness.request(None));
    assert!(matches!(
        error,
        BaseImageError::MissingRuntimeBinary {
            guest_path: CRUN_GUEST_PATH
        }
    ));
}

#[test]
fn scratch_files_never_land_in_the_worktree() {
    let harness = Harness::new("FROM alpine:3.21 AS api\n");
    let tree = fixture_tree(harness.root(), Some(ALPINE_OS_RELEASE), true, &[]);
    let (builder, _) = builder(&tree);
    let request = harness.request(None);
    let _ = expect_error(&builder, &request);
    assert!(
        std::fs::read_dir(&harness.context)
            .expect("context")
            .all(|entry| entry.expect("entry").file_name() == "Dockerfile"),
        "the builder must never write Dockerfile stages into the project context"
    );
}

/// Test-only [`BaseImageBuilder`] used to prove the shared conformance suite accepts a second
/// implementation (the production builder is exercised by `critical_rootfs_live` and above).
struct FakeBaseImageBuilder;

impl BaseImageBuilder for FakeBaseImageBuilder {
    fn materialize(
        &self,
        request: &WorldRootfsRequest,
        on_event: &mut dyn FnMut(RootfsBuildEvent),
    ) -> Result<WorldRootfs, BaseImageError> {
        let app_rootfs = request.state_dir.join("fake/rootfs/app");
        for guest in [CRUN_GUEST_PATH, WORLD_INIT_GUEST_PATH] {
            let path = app_rootfs.join(guest.trim_start_matches('/'));
            std::fs::create_dir_all(path.parent().expect("parent")).expect("fake bin dir");
            std::fs::write(&path, b"fake").expect("fake binary");
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755))
                .expect("fake mode");
        }
        let mut service_images = std::collections::BTreeMap::new();
        for key in &request.services {
            let digest = format!("sha256:{}", "b".repeat(64));
            service_images.insert(
                key.clone(),
                OciRootfs {
                    digest,
                    rootfs: request.state_dir.join("fake/oci/rootfs"),
                    config: request.state_dir.join("fake/oci/config.json"),
                },
            );
        }
        let digest =
            slipfork_engine::rootfs::RootfsDigest::from_hex("c".repeat(64)).expect("valid digest");
        on_event(RootfsBuildEvent::Installed {
            digest: digest.clone(),
            app_rootfs: app_rootfs.clone(),
        });
        Ok(WorldRootfs {
            digest,
            app_rootfs,
            crun_bin: PathBuf::from(CRUN_GUEST_PATH),
            service_images,
            metadata: ImageContractMetadata {
                os_release: OsRelease {
                    id: "alpine".to_owned(),
                    version_id: "3.21".to_owned(),
                    version_codename: String::new(),
                },
                elf_abi: slipfork_engine::rootfs::ElfAbi {
                    arch: "x86_64".to_owned(),
                    libc: slipfork_engine::rootfs::LibcFamily::Musl,
                    libc_version: "1.2".to_owned(),
                },
                exposed_ports: [8000].into_iter().collect(),
            },
            host_caches: request.host_caches.mounts.clone(),
        })
    }
}

#[test]
fn fake_base_image_builder_passes_the_shared_conformance_suite() {
    let harness = Harness::new("FROM alpine:3.21 AS api\nEXPOSE 8000\n");
    let mut request = harness.request(None);
    request.services = vec![
        BackingServiceKey::Postgres {
            version: "18".to_owned(),
        },
        BackingServiceKey::SeaweedFs,
    ];
    request.host_caches = HostCacheRequest {
        host_shell: PathBuf::from("/bin/sh"),
        mounts: vec![slipfork_engine::WorldMount {
            host_path: PathBuf::from("/home/remy/.cargo/registry"),
            guest_path: PathBuf::from("/.slipfork/host-cache/cargo/registry"),
            mode: MountMode::ReadOnly,
        }],
    };
    let mut events = Vec::new();
    let rootfs = FakeBaseImageBuilder
        .materialize(&request, &mut |event| events.push(event))
        .expect("fake builder");
    assert_world_rootfs_conformance(&rootfs, &request);
    assert!(matches!(
        events.last(),
        Some(RootfsBuildEvent::Installed { .. })
    ));
    assert_eq!(rootfs.host_caches, request.host_caches.mounts);

    let mut without_caches = request.clone();
    without_caches.host_caches = HostCacheRequest::default();
    let rootfs = FakeBaseImageBuilder
        .materialize(&without_caches, &mut |_| {})
        .expect("fake builder");
    assert!(rootfs.host_caches.is_empty());
}
