//! The single production [`BaseImageBuilder`] (`ADR-0009` §1–§3).
//!
//! Flow: parse the selected `Dockerfile` (never rewriting it), resolve `world.stage`, check the
//! static contract rules, compute the `RootfsDigest`, and — on a cache miss — write a scratch
//! `Dockerfile` **outside the worktree** that only appends `FROM <stage> AS slipfork-world` and
//! copies the two static `/.slipfork/bin` payloads from a named build context. `docker buildx`
//! exports the target as a plain local tree, which is validated (distro, `/bin/sh`, backing-service
//! daemons, pin digests) and atomically installed under `<state_dir>/cache/rootfs/<digest>/app`.
//!
//! All host capabilities are injected: the builder itself never touches `std::env` or spawns a
//! process.

#![forbid(unsafe_code)]

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use crate::CacheOutcome;
use crate::clock::MonotonicClock;
use crate::clock::elapsed_millis;
use crate::rootfs::abi::AbiProbe;
use crate::rootfs::buildx::{BuildxRequest, SLIPFORK_RUNTIME_CONTEXT};
use crate::rootfs::cache::{self, CacheLayout};
use crate::rootfs::contract;
use crate::rootfs::dockerfile::{Dockerfile, Stage};
use crate::rootfs::pin::{buildx_platform, crun_pin};
use crate::rootfs::{
    BaseImageBuilder, BaseImageError, CRUN_GUEST_PATH, ImageContractMetadata, OciImageStore,
    RootfsBuildEvent, RootfsDigest, RootfsDigestInputs, WORLD_INIT_GUEST_PATH, WorldRootfs,
    WorldRootfsRequest, sha256_hex, unique_suffix,
};
use crate::rootfs::{BuildxExporter, InputHasher};

/// Production [`BaseImageBuilder`]: `Dockerfile` stage -> verified local rootfs tree.
pub struct DockerfileStageBuilder {
    exporter: Arc<dyn BuildxExporter>,
    input_hasher: Arc<dyn InputHasher>,
    oci_store: Arc<dyn OciImageStore>,
    abi_probe: Arc<dyn AbiProbe>,
    clock: Arc<dyn MonotonicClock>,
}

impl DockerfileStageBuilder {
    /// Wire the builder with every host capability it needs (`ADR-0009` Composition Root).
    ///
    /// `clock` is the composition root's single [`MonotonicClock`]: the builder stamps the measured
    /// rootfs build duration with it, so the value it returns on [`WorldRootfs::millis`] needs no
    /// second time source downstream.
    pub fn new(
        exporter: Arc<dyn BuildxExporter>,
        input_hasher: Arc<dyn InputHasher>,
        oci_store: Arc<dyn OciImageStore>,
        abi_probe: Arc<dyn AbiProbe>,
        clock: Arc<dyn MonotonicClock>,
    ) -> Self {
        Self {
            exporter,
            input_hasher,
            oci_store,
            abi_probe,
            clock,
        }
    }

    fn materialize_services(
        &self,
        request: &WorldRootfsRequest,
        on_event: &mut dyn FnMut(RootfsBuildEvent),
    ) -> Result<BTreeMap<crate::rootfs::BackingServiceKey, crate::rootfs::OciRootfs>, BaseImageError>
    {
        let mut images = BTreeMap::new();
        for image in &request.services {
            on_event(RootfsBuildEvent::MaterializingService {
                image: image.clone(),
            });
            let rootfs = self.oci_store.materialize(image)?;
            images.insert(image.clone(), rootfs);
        }
        Ok(images)
    }

    /// Build one digest into a staging tree and install it atomically.
    #[allow(clippy::too_many_arguments)]
    fn build_app(
        &self,
        request: &WorldRootfsRequest,
        dockerfile_bytes: &[u8],
        stage: &Stage,
        stage_name: &str,
        digest: &RootfsDigest,
        layout: &CacheLayout,
        crun_bytes: &[u8],
        world_init_bytes: &[u8],
        on_event: &mut dyn FnMut(RootfsBuildEvent),
    ) -> Result<ImageContractMetadata, BaseImageError> {
        let final_dir = layout.rootfs_dir(digest);
        std::fs::create_dir_all(&final_dir).map_err(|error| io_error(&final_dir, &error))?;
        let staging = final_dir.join(format!(".staging-{}", unique_suffix(self.clock.as_ref())));
        let scratch = layout
            .tmp_dir()
            .join(format!("build-{}", unique_suffix(self.clock.as_ref())));
        let runtime_context = scratch.join("runtime");
        let _scratch_guard = Scratch::new(scratch.clone());
        std::fs::create_dir_all(&runtime_context)
            .map_err(|error| io_error(&runtime_context, &error))?;

        let scratch_dockerfile = scratch_dockerfile(dockerfile_bytes, stage_name);
        let dockerfile_path = scratch.join("Dockerfile");
        std::fs::write(&dockerfile_path, &scratch_dockerfile)
            .map_err(|error| io_error(&dockerfile_path, &error))?;
        write_executable(&runtime_context.join("crun"), crun_bytes)?;
        write_executable(
            &runtime_context.join("slipfork-world-init"),
            world_init_bytes,
        )?;

        let platform = buildx_platform(&request.target_arch)?;
        self.exporter.build_local(&BuildxRequest {
            dockerfile: &dockerfile_path,
            context_dir: &request.context_dir,
            target: "slipfork-world",
            dest_dir: &staging,
            build_envs: &request.build_envs,
            platform,
            runtime_context: &runtime_context,
        })?;

        let (os_release, elf_abi, exposed_ports) =
            contract::validate_extracted(request, stage, &staging, self.abi_probe.as_ref())?;
        let metadata = ImageContractMetadata {
            os_release,
            elf_abi,
            exposed_ports,
        };
        on_event(RootfsBuildEvent::ContractValidated {
            digest: digest.clone(),
        });

        // Atomic install: a sibling builder may have won the same digest meanwhile.
        let app = final_dir.join("app");
        match std::fs::rename(&staging, &app) {
            Ok(()) => {}
            Err(error) => {
                cache::remove_dir_all(&staging);
                if app.is_dir() && cache::read_manifest(&final_dir).is_some() {
                    return Ok(cache::read_manifest(&final_dir).expect("checked manifest presence"));
                }
                return Err(io_error(&app, &error));
            }
        }
        cache::write_manifest(&final_dir, &metadata, self.clock.as_ref())?;
        Ok(metadata)
    }
}

impl BaseImageBuilder for DockerfileStageBuilder {
    fn materialize(
        &self,
        request: &WorldRootfsRequest,
        on_event: &mut dyn FnMut(RootfsBuildEvent),
    ) -> Result<WorldRootfs, BaseImageError> {
        validate_request(request)?;
        let layout = CacheLayout::new(&request.state_dir)?;

        let dockerfile_bytes =
            std::fs::read(&request.dockerfile).map_err(|error| BaseImageError::Io {
                path: request.dockerfile.display().to_string(),
                message: error.to_string(),
            })?;
        let dockerfile = Dockerfile::parse(&dockerfile_bytes)?;
        let stage = dockerfile.target(request.target_stage.as_deref())?;
        let stage_name = stage
            .name
            .clone()
            .expect("target stage is named by construction");
        contract::validate_static(request, stage)?;

        let pin = *crun_pin(&request.target_arch)?;
        let crun_bytes = read_asset(&request.runtime.crun)?;
        let world_init_bytes = read_asset(&request.runtime.world_init)?;

        let context_merkle = self.input_hasher.hash_context(&request.context_dir)?;
        let dockerfile_rel = dockerfile_rel_path(&request.context_dir, &request.dockerfile);
        let world_init_sha256 = sha256_hex(&world_init_bytes);
        let digest = RootfsDigest::compute(&RootfsDigestInputs {
            image_id: &request.image_id,
            dockerfile_rel_path: &dockerfile_rel,
            dockerfile_bytes: &dockerfile_bytes,
            context_rel_path: &request.context_label,
            context_merkle_sha256: &context_merkle,
            target_stage: &stage_name,
            build_envs: &request.build_envs,
            crun_binary_sha256: pin.sha256,
            world_init_sha256: &world_init_sha256,
            target_arch: &request.target_arch,
        });

        let final_app = layout.app_rootfs(&digest);
        let started = self.clock.now();
        let (metadata, cache) = match cache::read_manifest(&layout.rootfs_dir(&digest)) {
            Some(metadata) if final_app.is_dir() => {
                on_event(RootfsBuildEvent::CacheHit {
                    digest: digest.clone(),
                });
                (metadata, CacheOutcome::Hit)
            }
            _ => {
                on_event(RootfsBuildEvent::CacheMiss {
                    digest: digest.clone(),
                });
                (
                    self.build_app(
                        request,
                        &dockerfile_bytes,
                        stage,
                        &stage_name,
                        &digest,
                        &layout,
                        &crun_bytes,
                        &world_init_bytes,
                        on_event,
                    )?,
                    CacheOutcome::Miss,
                )
            }
        };
        // A cache hit ran no build, so it records no duration: `None` is "not measured", never `0`.
        let millis = match cache {
            CacheOutcome::Hit => None,
            CacheOutcome::Miss => Some(elapsed_millis(started, self.clock.now())),
        };

        let host_caches = contract::validate_host_caches(
            request,
            stage,
            &metadata.elf_abi,
            &metadata.os_release.distro_label(),
            self.abi_probe.as_ref(),
        )?;
        let service_images = self.materialize_services(request, on_event)?;

        on_event(RootfsBuildEvent::Installed {
            digest: digest.clone(),
            app_rootfs: final_app.clone(),
        });
        Ok(WorldRootfs {
            digest,
            app_rootfs: final_app,
            crun_bin: PathBuf::from(CRUN_GUEST_PATH),
            service_images,
            metadata,
            host_caches,
            cache,
            millis,
        })
    }
}

/// Reject a request no builder may materialize (fail before any cache directory is created).
fn validate_request(request: &WorldRootfsRequest) -> Result<(), BaseImageError> {
    if request.image_id.trim().is_empty() {
        return Err(BaseImageError::NoBuildableImage);
    }
    for (label, path) in [
        ("context_dir", &request.context_dir),
        ("dockerfile", &request.dockerfile),
    ] {
        if !path.is_absolute() {
            return Err(BaseImageError::InvalidRequest {
                reason: format!("{label} '{}' must be absolute", path.display()),
            });
        }
    }
    if !request.context_dir.is_dir() {
        return Err(BaseImageError::InvalidRequest {
            reason: format!(
                "context_dir '{}' is not a directory",
                request.context_dir.display()
            ),
        });
    }
    if !request.dockerfile.is_file() {
        return Err(BaseImageError::InvalidRequest {
            reason: format!(
                "Dockerfile '{}' does not exist",
                request.dockerfile.display()
            ),
        });
    }
    if request.target_arch.trim().is_empty() {
        return Err(BaseImageError::InvalidRequest {
            reason: "target_arch must name a Linux architecture".to_owned(),
        });
    }
    if request
        .target_stage
        .as_deref()
        .is_some_and(|stage| stage.trim().is_empty())
    {
        return Err(BaseImageError::InvalidRequest {
            reason: "target_stage must be a stage name or absent, never empty".to_owned(),
        });
    }
    Ok(())
}

/// The scratch `Dockerfile`: user bytes verbatim plus one machine-generated stage.
fn scratch_dockerfile(dockerfile_bytes: &[u8], stage_name: &str) -> Vec<u8> {
    let mut bytes = dockerfile_bytes.to_vec();
    if !bytes.ends_with(b"\n") {
        bytes.push(b'\n');
    }
    bytes.extend_from_slice(
        format!(
            "# --- appended by Slipfork (never written to the repository) ---\n\
             FROM {stage_name} AS slipfork-world\n\
             COPY --from={SLIPFORK_RUNTIME_CONTEXT} /crun {CRUN_GUEST_PATH}\n\
             COPY --from={SLIPFORK_RUNTIME_CONTEXT} /slipfork-world-init {WORLD_INIT_GUEST_PATH}\n"
        )
        .as_bytes(),
    );
    bytes
}

fn dockerfile_rel_path(context_dir: &Path, dockerfile: &Path) -> String {
    dockerfile
        .strip_prefix(context_dir)
        .unwrap_or_else(|_| dockerfile.file_name().map(Path::new).unwrap_or(dockerfile))
        .to_string_lossy()
        .replace('\\', "/")
}

fn read_asset(path: &Path) -> Result<Vec<u8>, BaseImageError> {
    std::fs::read(path).map_err(|error| io_error(path, &error))
}

fn write_executable(path: &Path, bytes: &[u8]) -> Result<(), BaseImageError> {
    std::fs::write(path, bytes).map_err(|error| io_error(path, &error))?;
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755))
        .map_err(|error| io_error(path, &error))
}

fn io_error(path: &Path, error: &std::io::Error) -> BaseImageError {
    BaseImageError::Io {
        path: path.display().to_string(),
        message: error.to_string(),
    }
}

/// Scratch directory removed on drop (success or failure): the worktree stays untouched.
struct Scratch {
    path: PathBuf,
}

impl Scratch {
    fn new(path: PathBuf) -> Self {
        Self { path }
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        cache::remove_dir_all(&self.path);
    }
}

#[cfg(test)]
mod tests {
    use super::{dockerfile_rel_path, scratch_dockerfile};
    use std::path::Path;

    #[test]
    fn scratch_dockerfile_appends_one_target_stage() {
        let bytes = scratch_dockerfile(b"FROM alpine:3.21 AS app", "app");
        let text = String::from_utf8(bytes).expect("utf8");
        assert!(text.starts_with("FROM alpine:3.21 AS app\n"));
        assert!(text.contains("FROM app AS slipfork-world"));
        assert!(text.contains("COPY --from=slipfork-runtime /crun /.slipfork/bin/crun"));
        assert!(text.contains(
            "COPY --from=slipfork-runtime /slipfork-world-init /.slipfork/bin/slipfork-world-init"
        ));
        assert!(!text.contains("apt-get"));
        assert!(!text.contains("apk add"));
    }

    #[test]
    fn dockerfile_relative_path_prefers_the_context_relative_form() {
        assert_eq!(
            dockerfile_rel_path(Path::new("/repo"), Path::new("/repo/docker/api.Dockerfile")),
            "docker/api.Dockerfile"
        );
        assert_eq!(
            dockerfile_rel_path(Path::new("/elsewhere"), Path::new("/repo/Dockerfile")),
            "Dockerfile"
        );
    }
}
