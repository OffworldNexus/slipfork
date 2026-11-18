//! Protected critical tests for the `ADR-0009` 6-rule World Image Contract.
//!
//! Every rule asserts its typed [`BaseImageError`] variant **and** the exact user-facing diagnostic
//! (contract prefix + `  fix:` line). Rules 1/3/4/6 are static and fail before any host work; rules
//! 2/5 plus `/.slipfork/bin` integrity run the concrete [`DockerBuildx`] exporter over a real
//! extracted tree and the concrete [`ElfProbe`] over the real host `/bin/sh`. Nothing is mocked:
//! trees are produced by Docker on disk under `~/.cache/slipfork`, never in RAM `tmpfs`.

pub mod support;

use std::path::PathBuf;

use slipfork_engine::rootfs::{
    BaseImageBuilder, BaseImageError, CRUN_GUEST_PATH, DockerBuildx, DockerImageStore,
    DockerfileStageBuilder, ElfProbe, FsInputHasher, HostCacheRequest, RootfsBuildEvent,
    RuntimeAssets, SnowPortContract, WorldRootfsRequest,
};
use tempfile::TempDir;

struct Harness {
    _dir: TempDir,
    dockerfile: PathBuf,
    context: PathBuf,
    state_dir: PathBuf,
}

impl Harness {
    fn new(dockerfile: &str) -> Self {
        let dir = support::disk_tempdir("contract-");
        let root = dir.path().to_path_buf();
        let context = root.join("context");
        std::fs::create_dir_all(&context).expect("context");
        let dockerfile_path = context.join("Dockerfile");
        std::fs::write(&dockerfile_path, dockerfile).expect("write Dockerfile");
        Self {
            _dir: dir,
            dockerfile: dockerfile_path,
            context,
            state_dir: root.join("state"),
        }
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
            runtime: RuntimeAssets {
                crun: support::pinned_crun(),
                world_init: PathBuf::from(env!("CARGO_BIN_EXE_slipfork-world-init")),
            },
            host_caches: HostCacheRequest::default(),
        }
    }

    /// The production builder over the real exporter, real disk image store, and real ELF probe.
    fn builder(&self) -> DockerfileStageBuilder {
        DockerfileStageBuilder::new(
            DockerBuildx::new("docker"),
            FsInputHasher,
            DockerImageStore::new(self.state_dir.clone()),
            ElfProbe,
        )
    }
}

/// Materialize `request` and return the fail-closed error plus every progress event.
fn materialize_error(
    builder: &DockerfileStageBuilder,
    request: &WorldRootfsRequest,
) -> (BaseImageError, Vec<RootfsBuildEvent>) {
    let mut events = Vec::new();
    match builder.materialize(request, &mut |event| events.push(event)) {
        Err(error) => (error, events),
        Ok(rootfs) => panic!("expected a fail-closed contract error, got {rootfs:?}"),
    }
}

fn expect_message(error: &BaseImageError, expected: &str) {
    assert_eq!(error.to_string(), expected, "got {error:?}");
}

/// Assert the cold cache emitted `CacheMiss` before `build_app` ran.
fn expect_cold_cache_attempt(events: &[RootfsBuildEvent]) {
    assert!(
        events
            .iter()
            .any(|event| matches!(event, RootfsBuildEvent::CacheMiss { .. })),
        "cold cache, build attempted (`CacheMiss` precedes `build_app`): {events:?}"
    );
}

#[test]
fn rule_1_unnamed_final_stage_reports_the_exact_diagnostic() {
    let harness = Harness::new("FROM alpine:3.21\n");
    let (error, _) = materialize_error(&harness.builder(), &harness.request(None));
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
    let (error, _) = materialize_error(&harness.builder(), &harness.request(Some("dev")));
    assert!(matches!(error, BaseImageError::UnknownTargetStage { .. }));
    expect_message(
        &error,
        "error: World Image Contract #1 violated: `world.stage = \"dev\"` not found in Dockerfile (declared stages: 'builder', 'runtime')\n  fix: set `world.stage = \"builder\"` in slipfork.toml",
    );
}

#[test]
fn rule_2_unsupported_distro_names_the_supported_table() {
    let harness = Harness::new(
        "FROM alpine:3.21 AS runtime\nRUN printf 'ID=fedora\\nVERSION_ID=41\\n' > /etc/os-release\n",
    );
    let (error, events) = materialize_error(&harness.builder(), &harness.request(None));
    expect_cold_cache_attempt(&events);
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
    let harness = Harness::new("FROM alpine:3.21 AS runtime\nRUN rm -f /etc/os-release /bin/sh\n");
    let (error, events) = materialize_error(&harness.builder(), &harness.request(None));
    expect_cold_cache_attempt(&events);
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
    let (error, _) = materialize_error(&harness.builder(), &harness.request(None));
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
    let (error, _) = materialize_error(&harness.builder(), &harness.request(None));
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
    let (error, _) = materialize_error(&harness.builder(), &harness.request(None));
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
    let harness = Harness::new(
        "FROM alpine:3.21 AS api\nRUN mkdir -p /usr/lib/postgresql/18/bin && cp /bin/busybox /usr/lib/postgresql/18/bin/postgres\n",
    );
    let (error, events) = materialize_error(&harness.builder(), &harness.request(None));
    expect_cold_cache_attempt(&events);
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
    let (error, _) = materialize_error(&harness.builder(), &harness.request(None));
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
    let (error, _) = materialize_error(&harness.builder(), &harness.request(None));
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
    // `/.slipfork/bin/crun` already exists as a directory, so the appended stage copies the file
    // *inside* it and the integrity check sees a non-file at the pinned guest path.
    let harness = Harness::new("FROM alpine:3.21 AS api\nRUN mkdir -p /.slipfork/bin/crun\n");
    let (error, events) = materialize_error(&harness.builder(), &harness.request(None));
    expect_cold_cache_attempt(&events);
    assert!(matches!(
        error,
        BaseImageError::MissingRuntimeBinary {
            guest_path: CRUN_GUEST_PATH
        }
    ));
}

#[test]
fn scratch_files_never_land_in_the_worktree() {
    // A Dockerfile that really builds, so the builder reaches the post-export path where the
    // scratch `Dockerfile` and runtime context could otherwise leak into the project context.
    let harness = Harness::new("FROM alpine:3.21 AS api\nRUN mkdir -p /.slipfork/bin/crun\n");
    let (error, events) = materialize_error(&harness.builder(), &harness.request(None));
    expect_cold_cache_attempt(&events);
    assert!(matches!(error, BaseImageError::MissingRuntimeBinary { .. }));
    let mut entries: Vec<String> = std::fs::read_dir(&harness.context)
        .expect("context")
        .map(|entry| {
            entry
                .expect("entry")
                .file_name()
                .to_string_lossy()
                .into_owned()
        })
        .collect();
    entries.sort();
    assert_eq!(
        entries,
        ["Dockerfile"],
        "the builder must never write Dockerfile stages or runtime payloads into the project context"
    );
}
