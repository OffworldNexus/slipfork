//! Production [`OciImageStore`] over the Docker CLI (`ADR-0009` §2.2, design §6).
//!
//! This is the Composition Root's host-capability adapter, not domain logic: it resolves an
//! official backing-service image to a registry digest, exports that exact digest into
//! `<state_dir>/cache/oci/<hex>/rootfs` (non-`tmpfs`, `$UID:$GID`-owned by construction), records
//! the upstream image config as `config.json`, and reuses the cached tree on every later call —
//! zero network or Docker work once a digest is on disk.
//!
//! The daemon never runs `docker` itself: only this injected [`OciImageStore`] implementation
//! spawns the CLI, exactly like [`slipfork_engine::rootfs::CliBuildxExporter`] does for builds.

#![forbid(unsafe_code)]

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use slipfork_engine::rootfs::{BackingServiceKey, BaseImageError, OciImageStore, OciRootfs};

/// File recording the tag -> digest resolution so later boots use the same bytes (`tags.json`).
const TAGS_FILE: &str = "tags.json";

/// Docker-CLI-backed [`OciImageStore`] rooted at one explicit Slipfork state directory.
#[derive(Debug, Clone)]
pub struct DockerOciImageStore {
    state_dir: PathBuf,
    docker: PathBuf,
}

impl DockerOciImageStore {
    /// Root the content-addressed cache under `<state_dir>/cache/oci`.
    pub fn new(state_dir: impl Into<PathBuf>) -> Self {
        Self {
            state_dir: state_dir.into(),
            docker: PathBuf::from("docker"),
        }
    }

    /// Replace the `docker` executable (conformance harnesses inject an explicit path).
    pub fn with_docker(mut self, docker: impl Into<PathBuf>) -> Self {
        self.docker = docker.into();
        self
    }

    fn cache_root(&self) -> PathBuf {
        self.state_dir.join("cache").join("oci")
    }

    fn image_reference(&self, image: &BackingServiceKey) -> String {
        match image.version() {
            Some(version) => format!("{}:{version}", image.image_repository()),
            // `ADR-0009` §2.2 names SeaweedFS by repository only; the first resolution records
            // the registry digest in `tags.json`, and every later boot pins that digest.
            None => format!("{}:latest", image.image_repository()),
        }
    }

    fn run(&self, args: &[&str]) -> Result<String, BaseImageError> {
        let output = Command::new(&self.docker)
            .args(args)
            .stdin(Stdio::null())
            .output()
            .map_err(|error| BaseImageError::Io {
                path: self.docker.display().to_string(),
                message: format!(
                    "cannot run `{} {}`: {error}; is Docker installed and on PATH?",
                    self.docker.display(),
                    args.join(" ")
                ),
            })?;
        if !output.status.success() {
            return Err(BaseImageError::Io {
                path: self.docker.display().to_string(),
                message: format!(
                    "`{} {}` exited with {}: {}",
                    self.docker.display(),
                    args.join(" "),
                    output.status,
                    String::from_utf8_lossy(&output.stderr).trim()
                ),
            });
        }
        Ok(String::from_utf8_lossy(&output.stdout).trim().to_owned())
    }

    /// Resolve `reference` to `sha256:<64 lowercase hex>`, pulling it once when absent.
    fn resolve_digest(&self, reference: &str) -> Result<String, BaseImageError> {
        let inspect = || {
            self.run(&[
                "image",
                "inspect",
                reference,
                "--format",
                "{{index .RepoDigests 0}}",
            ])
        };
        let mut repo_digest = inspect().unwrap_or_default();
        if repo_digest.is_empty() {
            self.run(&["pull", "-q", reference])?;
            repo_digest = inspect()?;
        }
        let digest = repo_digest
            .rsplit_once('@')
            .map(|(_, digest)| digest.to_owned())
            .filter(|digest| {
                digest.strip_prefix("sha256:").is_some_and(|hex| {
                    hex.len() == 64 && hex.bytes().all(|byte| byte.is_ascii_hexdigit())
                })
            })
            .ok_or_else(|| BaseImageError::Io {
                path: reference.to_owned(),
                message: format!(
                    "{reference} resolved to '{repo_digest}', not a digest-pinned sha256 reference; \
                     fix: pull the official digest-pinned image"
                ),
            })?;
        Ok(digest)
    }

    fn record_tag(&self, reference: &str, digest: &str) {
        let path = self.cache_root().join(TAGS_FILE);
        let mut tags: std::collections::BTreeMap<String, String> = std::fs::read(&path)
            .ok()
            .and_then(|bytes| serde_json::from_slice(&bytes).ok())
            .unwrap_or_default();
        tags.insert(reference.to_owned(), digest.to_owned());
        if let Ok(bytes) = serde_json::to_vec_pretty(&tags) {
            let _ = std::fs::write(path, bytes);
        }
    }
}

impl OciImageStore for DockerOciImageStore {
    fn materialize(&self, image: &BackingServiceKey) -> Result<OciRootfs, BaseImageError> {
        let reference = self.image_reference(image);
        let digest = self.resolve_digest(&reference)?;
        let hex = digest
            .strip_prefix("sha256:")
            .expect("resolve_digest validates the sha256: prefix");
        let cache = self.cache_root().join(hex);
        let rootfs = cache.join("rootfs");
        let config = cache.join("config.json");
        std::fs::create_dir_all(self.cache_root()).map_err(|error| BaseImageError::Io {
            path: self.cache_root().display().to_string(),
            message: error.to_string(),
        })?;
        if !rootfs.is_dir() || !config.is_file() {
            self.export(&reference, &digest, &rootfs, &config)?;
        }
        self.record_tag(&reference, &digest);
        Ok(OciRootfs {
            digest,
            rootfs,
            config,
        })
    }
}

impl DockerOciImageStore {
    /// Export `reference@digest` into `rootfs`, atomically replacing a partial cache entry.
    fn export(
        &self,
        reference: &str,
        digest: &str,
        rootfs: &Path,
        config: &Path,
    ) -> Result<(), BaseImageError> {
        let pinned = match reference.rsplit_once(':') {
            Some((repository, _)) => format!("{repository}@{digest}"),
            None => format!("{reference}@{digest}"),
        };
        let staging = rootfs.with_extension(format!("staging-{}", std::process::id()));
        let archive = rootfs.with_extension(format!("tar-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&staging);
        std::fs::create_dir_all(&staging).map_err(|error| BaseImageError::Io {
            path: staging.display().to_string(),
            message: error.to_string(),
        })?;
        let container = self.run(&["create", &pinned])?;
        let exported = self.run(&["export", "-o", &archive.display().to_string(), &container]);
        let _ = self.run(&["rm", "-f", &container]);
        exported?;
        let status = Command::new("tar")
            .args(["-xf"])
            .arg(&archive)
            .arg("-C")
            .arg(&staging)
            .stdin(Stdio::null())
            .status()
            .map_err(|error| BaseImageError::Io {
                path: "tar".to_owned(),
                message: format!("cannot extract {pinned}: {error}"),
            })?;
        let _ = std::fs::remove_file(&archive);
        if !status.success() {
            return Err(BaseImageError::Io {
                path: archive.display().to_string(),
                message: format!("tar extraction of {pinned} failed with {status}"),
            });
        }
        let image_config =
            self.run(&["image", "inspect", &pinned, "--format", "{{json .Config}}"])?;
        let _ = std::fs::remove_dir_all(rootfs);
        std::fs::rename(&staging, rootfs).map_err(|error| BaseImageError::Io {
            path: rootfs.display().to_string(),
            message: error.to_string(),
        })?;
        std::fs::write(config, image_config).map_err(|error| BaseImageError::Io {
            path: config.display().to_string(),
            message: error.to_string(),
        })
    }
}
