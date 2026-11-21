//! Image collaborator: `from` pulls and `Dockerfile` builds into sealed, read-only rootfs clones.
//!
//! The cache owns every OCI and build handle, so a black-box image build never holds the pod
//! registry and a pod launch never waits behind a long `RUN` step.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use slipfork_forkfile::ast::Forkfile;
use slipfork_image::{
    Architecture, BuildArgs, BuildRequest, Credentials, ImageLayout, ImageReference,
    NativeImageBuilder, OciPuller, PulledImage, StepHost, StepLayout,
};
use slipfork_world::{BootState, ImageNode, StoreLayout, WorldEngine, WorldName};

use crate::world::supervise::SharedLog;

/// One sealed image plus the OCI metadata every pod derived from it inherits.
#[derive(Clone)]
pub(super) struct BuiltImage {
    /// Read-only rootfs subvolume drawn from the cache.
    pub(super) rootfs: PathBuf,
    /// Image `config.Env` entries, inherited verbatim.
    pub(super) inherited_env: Vec<String>,
    /// `ENTRYPOINT` followed by `CMD`; empty when the image defines neither.
    pub(super) argv: Vec<String>,
    /// Image `config.WorkingDir`.
    pub(super) workdir: String,
}

/// The sealed-image cache plus its puller and native builder.
pub(super) struct ImageCache {
    world: WorldName,
    project: PathBuf,
    doc: Forkfile,
    puller: OciPuller,
    builder: NativeImageBuilder,
    images: Vec<ImageNode>,
    sealed: BTreeMap<String, BuiltImage>,
    pulled: BTreeMap<String, PulledImage>,
}

impl ImageCache {
    pub(super) fn new(
        engine: Arc<dyn WorldEngine>,
        layout: &StoreLayout,
        world: WorldName,
        project: &Path,
        doc: &Forkfile,
        images: Vec<ImageNode>,
        architecture: Architecture,
        agent: &str,
        host: StepHost,
    ) -> Self {
        let credentials = Credentials::Anonymous;
        let images_root = layout.images_root();
        let puller = OciPuller::new(
            Arc::clone(&engine),
            ImageLayout::new(images_root.clone()),
            credentials.clone(),
            architecture,
            agent.to_owned(),
        );
        let build_puller = OciPuller::new(
            Arc::clone(&engine),
            ImageLayout::new(images_root.clone()),
            credentials,
            architecture,
            agent.to_owned(),
        );
        let builder = NativeImageBuilder::new(
            engine,
            build_puller,
            StepLayout::new(images_root),
            architecture,
            host,
        );
        Self {
            world,
            project: project.to_path_buf(),
            doc: doc.clone(),
            puller,
            builder,
            images,
            sealed: BTreeMap::new(),
            pulled: BTreeMap::new(),
        }
    }

    /// Seals one image and reports whether it was cached, pulled, or built.
    pub(super) fn build(
        &mut self,
        image: &str,
        log: &SharedLog,
    ) -> Result<(BuiltImage, BootState), String> {
        if let Some(built) = self.sealed.get(image) {
            return Ok((built.clone(), BootState::Cached));
        }
        let node = self
            .images
            .iter()
            .find(|candidate| candidate.id == image)
            .cloned()
            .ok_or_else(|| format!("unknown image `{image}`"))?;
        let mut out = log.clone();
        let (built, state) = if let Some(reference) = &node.from {
            let parsed = ImageReference::parse(reference).map_err(|error| error.to_string())?;
            log.note(format!("pull {reference}"));
            let pulled = self
                .puller
                .pull_into(&parsed, &mut out)
                .map_err(|error| error.to_string())?;
            let mut argv = pulled.facts.entrypoint.clone();
            argv.extend(pulled.facts.cmd.iter().cloned());
            (
                BuiltImage {
                    rootfs: pulled.rootfs,
                    inherited_env: pulled.facts.env,
                    argv,
                    workdir: "/".to_owned(),
                },
                BootState::Pulled,
            )
        } else {
            let context = self.project.join(&node.directory);
            let dockerfile = context.join(
                node.dockerfile
                    .clone()
                    .unwrap_or_else(|| PathBuf::from("Dockerfile")),
            );
            let mut args = BuildArgs::new();
            for (key, value) in self.doc.eval_image_args(self.world.as_str(), image) {
                args.declare(key, value);
            }
            let request = BuildRequest {
                context,
                dockerfile,
                args,
                target: self.doc.eval_image_stage(image),
            };
            let report = self
                .builder
                .build(&request, &mut out)
                .map_err(|error| error.to_string())?;
            log.note(format!(
                "dockerfile {image}: {} cached, {} built",
                report.cache_hits, report.cache_misses
            ));
            let state = if report.cache_misses == 0 && report.cache_hits > 0 {
                BootState::Cached
            } else {
                BootState::Built
            };
            (
                BuiltImage {
                    rootfs: report.rootfs,
                    inherited_env: report
                        .config
                        .env
                        .iter()
                        .map(|(key, value)| format!("{key}={value}"))
                        .collect(),
                    argv: report.config.exec_argv(),
                    workdir: report.config.workdir,
                },
                state,
            )
        };
        self.sealed.insert(image.to_owned(), built.clone());
        Ok((built, state))
    }

    /// Pulls one resource image (`postgres:18`, `redis:7`) once and reuses it afterwards.
    pub(super) fn pull(&mut self, reference: &str, log: &SharedLog) -> Result<PulledImage, String> {
        if let Some(image) = self.pulled.get(reference) {
            return Ok(image.clone());
        }
        let parsed = ImageReference::parse(reference).map_err(|error| error.to_string())?;
        log.note(format!("pull {reference}"));
        let mut out = log.clone();
        let image = self
            .puller
            .pull_into(&parsed, &mut out)
            .map_err(|error| error.to_string())?;
        self.pulled.insert(reference.to_owned(), image.clone());
        Ok(image)
    }
    }

    /// Resolved rootfs subvolume per image id, recorded in the world stamp.
    pub(super) fn paths(&self) -> BTreeMap<String, String> {
        self.sealed
            .iter()
            .map(|(image, built)| (image.clone(), built.rootfs.display().to_string()))
            .collect()
    }
}
