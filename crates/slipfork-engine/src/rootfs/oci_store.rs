//! Concrete Docker-CLI image store for backing-service rootfs extraction (`ADR-0009` §2.2).
//!
//! [`DockerImageStore`] resolves an official backing-service image to a registry digest, exports
//! that exact digest into `<state_dir>/cache/oci/<hex>/rootfs` (non-`tmpfs`, `$UID:$GID`-owned by
//! construction), records the upstream image config as `config.json`, and reuses the cached tree on
//! every later call — zero network or Docker work once a digest is on disk.
//!
//! `docker pull`, `docker export`, and `tar -xf` stream every line to the caller (a `pull -q` would
//! hide a multi-second download). The builder never runs `docker` itself: [`DockerImageStore`] is
//! the only structure that spawns the CLI, exactly like [`crate::rootfs::DockerBuildx`] does for
//! builds. Both are plain structs wired by the Composition Root and passed by value.

#![forbid(unsafe_code)]

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use crate::clock::elapsed_millis;
use crate::stream::ChildCommand;
use slipfork_world::CacheOutcome;
use slipfork_world::rootfs::{BackingServiceKey, BaseImageError, OciRootfs};

/// File recording the tag -> digest resolution so later boots use the same bytes (`tags.json`).
const TAGS_FILE: &str = "tags.json";

/// Docker-CLI-backed image store rooted at one explicit Slipfork state directory.
#[derive(Debug, Clone)]
pub struct DockerImageStore {
    state_dir: PathBuf,
    docker: PathBuf,
}

impl DockerImageStore {
    /// Root the content-addressed cache under `<state_dir>/cache/oci`.
    ///
    /// A cache miss takes the two `Instant`s around its own export and records their elapsed
    /// milliseconds; no clock is injected.
    pub fn new(state_dir: impl Into<PathBuf>) -> Self {
        Self {
            state_dir: state_dir.into(),
            docker: PathBuf::from("docker"),
        }
    }

    /// Materialize `image` under `<state_dir>/cache/oci/<sha256>` (or return the cached copy).
    pub fn materialize(
        &self,
        image: &BackingServiceKey,
        on_line: &mut dyn FnMut(&str),
    ) -> Result<OciRootfs, BaseImageError> {
        self.materialize_reference(&self.image_reference(image), on_line)
    }

    /// Materialize one image reference (tag or `repository@sha256:<hex>`) into the cache.
    pub fn materialize_reference(
        &self,
        reference: &str,
        on_line: &mut dyn FnMut(&str),
    ) -> Result<OciRootfs, BaseImageError> {
        let digest = self.resolve_digest(reference, on_line)?;
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
            let started = std::time::Instant::now();
            self.export(reference, &digest, &rootfs, &config, on_line)?;
            self.record_tag(reference, &digest);
            return Ok(OciRootfs {
                digest,
                rootfs,
                config,
                cache: CacheOutcome::Miss,
                millis: Some(elapsed_millis(started, std::time::Instant::now())),
            });
        }
        self.record_tag(reference, &digest);
        // The cached tree was reused: no export ran, so no duration is recorded (`None`, never `0`).
        Ok(OciRootfs {
            digest,
            rootfs,
            config,
            cache: CacheOutcome::Hit,
            millis: None,
        })
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

    /// Run one `docker` verb, streaming its lines to `on_line` when given, and return its stdout.
    fn run(
        &self,
        args: &[&str],
        on_line: Option<&mut dyn FnMut(&str)>,
    ) -> Result<String, BaseImageError> {
        let command = ChildCommand::new(self.docker.clone(), owned_args(args));
        let invoked = || {
            format!(
                "cannot run `{} {}`; is Docker installed and on PATH?",
                self.docker.display(),
                args.join(" ")
            )
        };
        let output = match on_line {
            Some(on_line) => command.stream(on_line),
            None => command.capture(),
        }
        .map_err(|error| BaseImageError::Io {
            path: self.docker.display().to_string(),
            message: format!("{} ({error})", invoked()),
        })?;
        if !output.success() {
            return Err(BaseImageError::Io {
                path: self.docker.display().to_string(),
                message: format!(
                    "`{} {}` exited with {}: {}",
                    self.docker.display(),
                    args.join(" "),
                    output.status,
                    output.tail.trim()
                ),
            });
        }
        Ok(output.stdout.trim().to_owned())
    }

    /// Resolve `reference` to `sha256:<64 lowercase hex>`, pulling it once when absent.
    fn resolve_digest(
        &self,
        reference: &str,
        on_line: &mut dyn FnMut(&str),
    ) -> Result<String, BaseImageError> {
        let mut repo_digest = self.inspect_repo_digest(reference)?;
        if repo_digest.is_none() {
            // No `-q`: a multi-second download must stream its layers instead of a silent wait.
            self.run(&["pull", reference], Some(&mut *on_line))?;
            repo_digest = self.inspect_repo_digest(reference)?;
        }
        let repo_digest = repo_digest.unwrap_or_default();
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

    /// The first `RepoDigests` entry, or `None` when the image is absent.
    ///
    /// A spawn failure propagates: turning "docker is not on PATH" into a `pull` failure would hide
    /// the real remedy behind a second, confusing error.
    fn inspect_repo_digest(&self, reference: &str) -> Result<Option<String>, BaseImageError> {
        let command = ChildCommand::new(
            self.docker.clone(),
            owned_args(&[
                "image",
                "inspect",
                reference,
                "--format",
                "{{index .RepoDigests 0}}",
            ]),
        );
        let output = command.capture().map_err(|error| BaseImageError::Io {
            path: self.docker.display().to_string(),
            message: format!(
                "cannot run `{} image inspect {reference}`; is Docker installed and on PATH? ({error})",
                self.docker.display()
            ),
        })?;
        if !output.success() {
            return Ok(None);
        }
        Ok(Some(output.stdout.trim().to_owned()))
    }

    fn record_tag(&self, reference: &str, digest: &str) {
        let path = self.cache_root().join(TAGS_FILE);
        let mut tags: BTreeMap<String, String> = std::fs::read(&path)
            .ok()
            .and_then(|bytes| serde_json::from_slice(&bytes).ok())
            .unwrap_or_default();
        tags.insert(reference.to_owned(), digest.to_owned());
        if let Ok(bytes) = serde_json::to_vec_pretty(&tags) {
            let _ = std::fs::write(path, bytes);
        }
    }

    /// Export `reference@digest` into `rootfs`, atomically replacing a partial cache entry.
    fn export(
        &self,
        reference: &str,
        digest: &str,
        rootfs: &Path,
        config: &Path,
        on_line: &mut dyn FnMut(&str),
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
        let container = self.run(&["create", pinned.as_str()], None)?;
        let archive_path = archive.display().to_string();
        let exported = self.run(
            &["export", "-o", archive_path.as_str(), container.as_str()],
            Some(&mut *on_line),
        );
        let _ = self.run(&["rm", "-f", container.as_str()], None);
        exported?;
        let extracted = ChildCommand::new(
            "tar",
            vec![
                "-xf".to_owned(),
                archive_path,
                "-C".to_owned(),
                staging.display().to_string(),
            ],
        )
        .stream(&mut *on_line)
        .map_err(|error| BaseImageError::Io {
            path: "tar".to_owned(),
            message: format!("cannot extract {pinned}: {error}"),
        })?;
        let _ = std::fs::remove_file(&archive);
        if !extracted.success() {
            return Err(BaseImageError::Io {
                path: archive.display().to_string(),
                message: format!(
                    "tar extraction of {pinned} failed with {}: {}",
                    extracted.status,
                    extracted.tail.trim()
                ),
            });
        }
        let image_config = self.run(
            &["image", "inspect", &pinned, "--format", "{{json .Config}}"],
            None,
        )?;
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

/// Own the argv a [`ChildCommand`] takes (it stores `String`s, one allocation per verb).
fn owned_args(args: &[&str]) -> Vec<String> {
    args.iter().map(|arg| (*arg).to_owned()).collect()
}
