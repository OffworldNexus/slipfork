//! The puller: resolve, stream, verify, unpack, seal, cache.

use std::fs;
use std::io::{self, Read, Write};
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use slipfork_world::{Dirtiness, ImageFacts, WorldEngine};

use crate::digest::DigestReader;
use crate::error::{Error, Result};
use crate::layout::ImageLayout;
use crate::manifest::ImageManifest;
use crate::reference::ImageReference;
use crate::registry::{Credentials, RegistryClient, RegistryScheme};
use crate::unpack::LayerUnpacker;
use crate::{Architecture, BaseImageBuilder};

/// A sealed read-only image subvolume ready to snapshot for containers.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PulledImage {
    /// Path of the sealed `images/oci/<digest>` subvolume.
    pub rootfs: PathBuf,
    /// `sha256:<hex>` of the per-architecture manifest.
    pub digest: String,
    /// Architecture the manifest list was resolved for.
    pub architecture: Architecture,
    /// `config.Env`, `config.Entrypoint`, and `config.Cmd`, inherited by resource pods.
    pub facts: ImageFacts,
}

/// The `{config:{Env,Entrypoint,Cmd}}` slice of a standard OCI image config.
#[derive(serde::Deserialize)]
struct ConfigDocument {
    #[serde(default)]
    config: ConfigRuntime,
}

#[derive(serde::Deserialize, Default)]
struct ConfigRuntime {
    #[serde(default, rename = "Env")]
    env: Vec<String>,
    #[serde(default, rename = "Entrypoint")]
    entrypoint: Vec<String>,
    #[serde(default, rename = "Cmd")]
    cmd: Vec<String>,
}

/// Docker-free OCI Distribution v2 puller behind the [`BaseImageBuilder`] port.
pub struct OciPuller {
    world: Arc<dyn WorldEngine>,
    layout: ImageLayout,
    architecture: Architecture,
    client: RegistryClient,
}

impl OciPuller {
    /// Builds the puller; the composition root injects every root and the architecture.
    #[must_use]
    pub fn new(
        world: Arc<dyn WorldEngine>,
        layout: ImageLayout,
        credentials: Credentials,
        architecture: Architecture,
        user_agent: String,
    ) -> Self {
        Self {
            world,
            layout,
            architecture,
            client: RegistryClient::new(credentials, user_agent),
        }
    }

    /// Uses `scheme` for registry base URLs (`Http` only for loopback lab registries).
    #[must_use]
    pub fn with_scheme(mut self, scheme: RegistryScheme) -> Self {
        self.client = self.client.with_scheme(scheme);
        self
    }

    /// Pulls `reference`, reusing a sealed cache entry without downloading a blob again.
    pub fn pull(&self, reference: &ImageReference) -> Result<PulledImage> {
        self.pull_into(reference, &mut io::sink())
    }

    /// Pulls `reference`, flushing one progress line per registry phase into `log`.
    ///
    /// The caller supplies the sink: the CLI forwards each line as a typed boot event, tests
    /// pass [`io::sink`]. Every line is flushed immediately so a slow layer never looks stalled.
    pub fn pull_into(
        &self,
        reference: &ImageReference,
        log: &mut dyn Write,
    ) -> Result<PulledImage> {
        // A recorded digest with a sealed rootfs means the whole pull is served from disk: no
        // registry session, no manifest request, and no "pull" line in the boot log.
        let key = reference.to_string();
        if let Some(digest) = self.layout.cached_ref(&key) {
            let image = self.layout.image(&digest);
            if image.exists() && self.world.is_sealed(&image)? {
                let path = self.layout.config(&digest);
                let bytes = fs::read(&path).map_err(|source| Error::io("read", &path, source))?;
                let facts = parse_config(&bytes, &path)?;
                note(log, format!("sealed {digest}: reusing cache"));
                return Ok(self.pulled(image, digest, facts));
            }
        }
        let mut session = self.client.session(reference);
        let resolved = session.manifest(self.architecture)?;
        let digest = resolved.digest.clone();
        self.layout.remember_ref(&key, &digest);
        note(
            log,
            format!(
                "manifest {digest}: {} layer(s)",
                resolved.manifest.layers.len()
            ),
        );
        self.layout.ensure()?;
        let facts = self.image_facts(&mut session, &resolved, log)?;
        let image = self.layout.image(&digest);
        if image.exists() {
            if self.world.is_sealed(&image)? {
                note(log, format!("sealed {digest}: reusing cache"));
                return Ok(self.pulled(image, digest, facts));
            }
            self.world.destroy(&image, Dirtiness::Dirty, true)?;
        }
        let staging = self.layout.staging(&digest);
        if staging.exists() {
            self.world.destroy(&staging, Dirtiness::Dirty, true)?;
        }
        self.world.create_subvolume(&staging)?;
        let outcome = self
            .unpack(&mut session, &resolved.manifest, &staging, log)
            .and_then(|()| self.world.seal(&staging).map_err(Error::from))
            .and_then(|()| {
                fs::rename(&staging, &image).map_err(|source| Error::io("install", &image, source))
            });
        if let Err(err) = outcome {
            let _ = self.world.destroy(&staging, Dirtiness::Dirty, true);
            return Err(err);
        }
        Ok(self.pulled(image, digest, facts))
    }

    fn pulled(&self, rootfs: PathBuf, digest: String, facts: ImageFacts) -> PulledImage {
        PulledImage {
            rootfs,
            digest,
            architecture: self.architecture,
            facts,
        }
    }

    /// Loads the image config from the cache, or fetches and verifies it once.
    fn image_facts(
        &self,
        session: &mut crate::registry::RegistrySession<'_>,
        resolved: &crate::registry::ResolvedManifest,
        log: &mut dyn Write,
    ) -> Result<ImageFacts> {
        let path = self.layout.config(&resolved.digest);
        if let Ok(bytes) = fs::read(&path) {
            return parse_config(&bytes, &path);
        }
        let descriptor = &resolved.manifest.config;
        let mut source = DigestReader::new(session.blob(&descriptor.digest)?);
        let mut bytes = Vec::new();
        source
            .read_to_end(&mut bytes)
            .map_err(|source_err| Error::io("read", &path, source_err))?;
        io::copy(&mut source, &mut io::sink())
            .map_err(|source_err| Error::io("drain", &path, source_err))?;
        let actual = source.digest();
        if actual != descriptor.digest {
            return Err(Error::DigestMismatch {
                subject: format!("image config {}", descriptor.digest),
                expected: descriptor.digest.clone(),
                actual,
            });
        }
        fs::write(&path, &bytes).map_err(|source_err| Error::io("write", &path, source_err))?;
        fs::set_permissions(&path, fs::Permissions::from_mode(0o644))
            .map_err(|source_err| Error::io("chmod", &path, source_err))?;
        note(log, format!("config {} cached", descriptor.digest));
        parse_config(&bytes, &path)
    }

    fn unpack(
        &self,
        session: &mut crate::registry::RegistrySession<'_>,
        manifest: &ImageManifest,
        root: &Path,
        log: &mut dyn Write,
    ) -> Result<()> {
        let unpacker = LayerUnpacker::new(root);
        let total = manifest.layers.len();
        for (index, layer) in manifest.layers.iter().enumerate() {
            let media_type =
                layer
                    .media_type
                    .as_deref()
                    .ok_or_else(|| Error::MissingMediaType {
                        digest: layer.digest.clone(),
                    })?;
            note(
                log,
                format!("layer {}/{total} {} streaming", index + 1, layer.digest),
            );
            let mut source = DigestReader::new(session.blob(&layer.digest)?);
            unpacker.apply(&mut source, media_type)?;
            // A tar reader may stop before the compressed trailer; hash the rest too.
            io::copy(&mut source, &mut io::sink())
                .map_err(|source_err| Error::io("drain", root, source_err))?;
            let actual = source.digest();
            if actual != layer.digest {
                return Err(Error::DigestMismatch {
                    subject: format!("layer {}", layer.digest),
                    expected: layer.digest.clone(),
                    actual,
                });
            }
            note(
                log,
                format!("layer {}/{total} {} verified", index + 1, layer.digest),
            );
        }
        Ok(())
    }
}

/// Parses a cached image config; a missing runtime block yields empty facts.
fn parse_config(bytes: &[u8], path: &Path) -> Result<ImageFacts> {
    let document: ConfigDocument = serde_json::from_slice(bytes).map_err(|source| Error::Json {
        url: path.display().to_string(),
        source,
    })?;
    Ok(ImageFacts {
        env: document.config.env,
        entrypoint: document.config.entrypoint,
        cmd: document.config.cmd,
    })
}

/// Writes one already-formatted progress line and flushes it; sink errors are not fatal.
fn note(log: &mut dyn Write, line: String) {
    let _ = writeln!(log, "{line}");
    let _ = log.flush();
}

impl BaseImageBuilder for OciPuller {
    fn pull(&self, reference: &ImageReference) -> Result<PulledImage> {
        OciPuller::pull(self, reference)
    }
}
