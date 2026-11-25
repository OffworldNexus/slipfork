//! The puller: resolve, stream, verify, unpack, seal, cache.

mod flight;

use std::fs;
use std::io::{self, Read};
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use self::flight::{FlightRole, FlightTable, PullGuard, await_flight, new_flight, new_table};

use slipfork_world::{Dirtiness, ImageFacts, WorldEngine, is_subvolume_path};

use crate::credentials::RegistryCredentials;
use crate::digest::DigestReader;
use crate::error::{Error, Result};
use crate::layout::ImageLayout;
use crate::manifest::ImageManifest;
use crate::reference::ImageReference;
use crate::registry::{RegistryClient, RegistryScheme};
use crate::step_log::{StepLog, StepPlace};
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
    /// The sealed subvolume was served from disk without any registry request.
    pub cached: bool,
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
    flights: FlightTable,
    attempts: AtomicUsize,
}

impl OciPuller {
    /// Builds the puller; the composition root injects every root and the architecture.
    #[must_use]
    pub fn new(
        world: Arc<dyn WorldEngine>,
        layout: ImageLayout,
        credentials: RegistryCredentials,
        architecture: Architecture,
        user_agent: String,
    ) -> Self {
        Self {
            world,
            layout,
            architecture,
            client: RegistryClient::new(credentials, user_agent),
            flights: new_table(),
            attempts: AtomicUsize::new(0),
        }
    }

    /// Shares this puller's in-flight table, so two pullers over one store coalesce in-process.
    #[must_use]
    pub fn sharing_flights(self, other: &Self) -> Self {
        Self {
            flights: Arc::clone(&other.flights),
            ..self
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
        self.pull_into(reference, &mut StepLog::discard())
    }

    /// Pulls `reference` as one live `FROM` row (1 of 1), labelled with `args` as the author wrote
    /// it; the normalized registry reference never reaches the row.
    pub fn pull_row(
        &self,
        reference: &ImageReference,
        args: &str,
        log: &mut StepLog<'_>,
    ) -> Result<PulledImage> {
        log.started(StepPlace::new(1, 1, 0), "FROM", args);
        let started = std::time::Instant::now();
        match self.pull_into(reference, log) {
            Ok(image) => {
                log.done(started.elapsed());
                Ok(image)
            }
            Err(error) => {
                log.failed(started.elapsed(), error.exit_code());
                Err(error)
            }
        }
    }

    /// Pulls `reference`, emitting one typed engine note per registry phase into `log`.
    pub fn pull_into(
        &self,
        reference: &ImageReference,
        log: &mut StepLog<'_>,
    ) -> Result<PulledImage> {
        // One real pull per OCI reference: two `Dockerfile` builds sharing a base wait here
        // instead of unpacking into the same staging directory.
        let key = reference.to_string();
        match self.join_flight(&key)? {
            FlightRole::Wait(flight) => match await_flight(&flight) {
                Ok(image) => Ok(image),
                // The leader failed: retry once against the cache it just cleaned up.
                Err(_) => self.pull_uncached(reference, log),
            },
            FlightRole::Lead(guard) => {
                let outcome = self.pull_uncached(reference, log);
                guard.finish(outcome.as_ref().cloned().map_err(|error| error.to_string()));
                outcome
            }
        }
    }

    /// Resolves `reference` once and returns its manifest digest plus OCI config facts.
    ///
    /// The digest is remembered in `images/refs.json`, so a later warm [`RefResolver`] resolves it
    /// with no registry request. The config document is cached beside the digest.
    ///
    /// [`RefResolver`]: crate::plan::RefResolver
    pub fn resolve_image(&self, reference: &ImageReference) -> Result<(String, ImageFacts)> {
        let mut session = self.client.session(reference);
        let resolved = session.manifest(self.architecture)?;
        let digest = resolved.digest.clone();
        self.layout.remember_ref(&reference.to_string(), &digest);
        self.layout.ensure()?;
        let mut log = StepLog::discard();
        let facts = self.image_facts(&mut session, &resolved, &mut log)?;
        Ok((digest, facts))
    }

    /// The OCI config a prior resolve cached for `digest`, when it is still readable.
    #[must_use]
    pub fn cached_facts(&self, digest: &str) -> Option<ImageFacts> {
        let path = self.layout.config(digest);
        let bytes = fs::read(&path).ok()?;
        parse_config(&bytes, &path).ok()
    }

    /// Streams and verifies `reference`'s layers directly into `slot` (a writable snapshot).
    ///
    /// No sealed `images/oci/<digest>` rootfs is installed: the slot becomes the `base`/`external`
    /// node. A cached config document avoids a second config download; the manifest and blobs are
    /// fetched through the injected registry client, which re-reads credentials per session.
    pub fn unpack_into(
        &self,
        reference: &ImageReference,
        slot: &Path,
        log: &mut StepLog<'_>,
    ) -> Result<PulledImage> {
        let mut session = self.client.session(reference);
        let resolved = session.manifest(self.architecture)?;
        let digest = resolved.digest.clone();
        self.layout.remember_ref(&reference.to_string(), &digest);
        self.layout.ensure()?;
        let facts = self.image_facts(&mut session, &resolved, log)?;
        self.unpack(&mut session, &resolved.manifest, slot, log)?;
        Ok(self.pulled(slot.to_path_buf(), digest, facts, false))
    }

    /// Streams and verifies `reference`'s planned manifest layers directly into `slot`.
    ///
    /// The build plan's digest is authoritative: the reference is pinned to it, so the registry
    /// serves the manifest **by digest** and a floating tag that moved between planning and
    /// materializing cannot change what is unpacked. The tag is only remembered as an alias of
    /// that digest for later warm resolves.
    pub fn unpack_digest_into(
        &self,
        reference: &ImageReference,
        expected_digest: &str,
        slot: &Path,
        log: &mut StepLog<'_>,
    ) -> Result<PulledImage> {
        let pinned = reference.with_digest(expected_digest)?;
        let mut session = self.client.session(&pinned);
        let resolved = match session.manifest(self.architecture) {
            Ok(resolved) => resolved,
            // Tag-only lab registries 404 on `manifests/sha256:…`; retry the tag but still require
            // the planned digest, so a tag that moved stays a hard `MovedTag` error.
            Err(Error::Http { status: 404, .. }) => {
                session = self.client.session(reference);
                session.manifest(self.architecture)?
            }
            Err(error) => return Err(error),
        };
        let digest = resolved.digest.clone();
        if digest != expected_digest {
            return Err(Error::MovedTag {
                reference: reference.to_string(),
                expected: expected_digest.to_owned(),
                actual: digest,
            });
        }
        self.layout.remember_ref(&reference.to_string(), &digest);
        self.layout.ensure()?;
        let facts = self.image_facts(&mut session, &resolved, log)?;
        self.unpack(&mut session, &resolved.manifest, slot, log)?;
        Ok(self.pulled(slot.to_path_buf(), digest, facts, false))
    }

    /// Joins or opens the singleflight slot for one reference.
    fn join_flight(&self, key: &str) -> Result<FlightRole> {
        let mut table = self.flights.lock().map_err(|_| Error::Interrupted {
            message: "the pull singleflight table was poisoned".to_owned(),
        })?;
        if let Some(existing) = table.get(key) {
            return Ok(FlightRole::Wait(Arc::clone(existing)));
        }
        let flight = new_flight();
        table.insert(key.to_owned(), Arc::clone(&flight));
        Ok(FlightRole::Lead(PullGuard::new(
            key.to_owned(),
            Arc::clone(&self.flights),
            flight,
        )))
    }

    /// The sealed image `reference` already pulls to, with no registry request at all.
    ///
    /// A recorded digest whose rootfs is still sealed means the whole pull is served from disk.
    /// The daemon probes this before announcing a `FROM` row, so a cache hit settles as one
    /// `cached` verdict with zero inner rows while a real pull opens its row first.
    pub fn cached(&self, reference: &ImageReference) -> Result<Option<PulledImage>> {
        let Some(digest) = self.layout.cached_ref(&reference.to_string()) else {
            return Ok(None);
        };
        let image = self.layout.image(&digest);
        if !image.exists() || !self.world.is_sealed(&image)? {
            return Ok(None);
        }
        let path = self.layout.config(&digest);
        let bytes = fs::read(&path).map_err(|source| Error::io("read", &path, source))?;
        let facts = parse_config(&bytes, &path)?;
        Ok(Some(self.pulled(image, digest, facts, true)))
    }

    /// The real pull, run by exactly one thread per reference at a time.
    fn pull_uncached(
        &self,
        reference: &ImageReference,
        log: &mut StepLog<'_>,
    ) -> Result<PulledImage> {
        // A recorded digest with a sealed rootfs means the whole pull is served from disk: no
        // registry session, no manifest request, and no "pull" line in the boot log.
        let key = reference.to_string();
        if let Some(image) = self.cached(reference)? {
            note(log, format!("sealed {}: reusing cache", image.digest));
            return Ok(image);
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
                return Ok(self.pulled(image, digest, facts, true));
            }
            self.discard(&image);
        }
        // A crashed attempt may have left a plain directory where a subvolume belongs; never let
        // that stale entry fail the next pull.
        for stale in self.layout.stale_staging(&digest) {
            self.discard(&stale);
        }
        let attempt = self.attempts.fetch_add(1, Ordering::Relaxed);
        let staging = self
            .layout
            .staging_attempt(&digest, std::process::id(), attempt);
        self.world.create_subvolume(&staging)?;
        let outcome = self
            .unpack(&mut session, &resolved.manifest, &staging, log)
            .and_then(|()| self.world.seal(&staging).map_err(Error::from))
            .and_then(|()| {
                fs::rename(&staging, &image).map_err(|source| Error::io("install", &image, source))
            });
        if let Err(err) = outcome {
            self.discard(&staging);
            return Err(err);
        }
        Ok(self.pulled(image, digest, facts, false))
    }

    /// Removes one crashed staging entry or stale image: a subvolume is destroyed, anything else
    /// is unlinked, and no cleanup failure ever aborts the pull.
    fn discard(&self, path: &Path) {
        if is_subvolume_path(path).unwrap_or(false) {
            let _ = self.world.destroy(path, Dirtiness::Dirty, true);
        } else if path.is_dir() {
            let _ = fs::remove_dir_all(path);
        } else {
            let _ = fs::remove_file(path);
        }
    }

    fn pulled(
        &self,
        rootfs: PathBuf,
        digest: String,
        facts: ImageFacts,
        cached: bool,
    ) -> PulledImage {
        PulledImage {
            rootfs,
            digest,
            architecture: self.architecture,
            facts,
            cached,
        }
    }

    /// Loads the image config from the cache, or fetches and verifies it once.
    fn image_facts(
        &self,
        session: &mut crate::registry::RegistrySession<'_>,
        resolved: &crate::registry::ResolvedManifest,
        log: &mut StepLog<'_>,
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
        log: &mut StepLog<'_>,
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

/// Reports one already-formatted bookkeeping note on `log`.
fn note(log: &mut StepLog<'_>, line: String) {
    log.note(&line);
}

impl BaseImageBuilder for OciPuller {
    fn pull(&self, reference: &ImageReference) -> Result<PulledImage> {
        OciPuller::pull(self, reference)
    }
}
