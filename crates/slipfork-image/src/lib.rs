//! Native OCI pulls and `Dockerfile` evaluation into cached read-only Btrfs subvolumes.
//!
//! [`OciPuller`] speaks the OCI Distribution API v2 directly over HTTPS: bearer-token auth,
//! multi-arch manifest-list resolution, streamed `sha256` verification, and tar+gzip/zstd
//! unpacking with whiteout semantics into a sealed read-only Btrfs subvolume.
//!
//! [`NativeImageBuilder`] evaluates multi-stage `Dockerfile`s natively: `FROM … AS`, non-secret
//! `ARG`, `ENV`, `WORKDIR`, `COPY [--from] [--chown]`, `ADD`, `RUN` (heredocs and
//! `--mount=type=cache`), `USER`, `EXPOSE`, `CMD`/`ENTRYPOINT`, and `.dockerignore`. Every
//! filesystem instruction is a writable Btrfs CoW snapshot sealed into `images/steps/<key>`, so a
//! repeat build is a cache hit instead of a rebuild. `RUN` executes through `crun` in an
//! unprivileged user namespace, never an external builder daemon.
//!
//! Every root, credential, architecture, and host fact is injected by the composition root, so
//! this crate never reads the environment and never binds a socket.
//!
//! ```no_run
//! use std::sync::Arc;
//!
//! use slipfork_image::{
//!     Architecture, BuildArgs, BuildRequest, ImageLayout, NativeImageBuilder, OciPuller,
//!     RegistryCredentials, StepHost, StepLayout, StepLog,
//! };
//! use slipfork_world::{BtrfsEngine, StoreLayout, WorldEngine};
//!
//! # fn main() -> Result<(), Box<dyn std::error::Error>> {
//! let store = StoreLayout::new("/mnt/btrfs");
//! let engine = Arc::new(BtrfsEngine::new(store.clone()));
//! engine.ensure_store()?;
//! let images = ImageLayout::new(store.images_root());
//! let puller = OciPuller::new(
//!     Arc::clone(&engine) as Arc<dyn WorldEngine>,
//!     images.clone(),
//!     RegistryCredentials::default(),
//!     Architecture::X86_64,
//!     "slipfork/0.1".to_owned(),
//! );
//! let host = StepHost::new("/usr/bin/crun", "/etc/resolv.conf", "/var/tmp/slipfork", 1000, 1000);
//! let builder = NativeImageBuilder::new(
//!     Arc::clone(&engine) as Arc<dyn WorldEngine>,
//!     puller,
//!     StepLayout::new(store.images_root()),
//!     Architecture::X86_64,
//!     host,
//! );
//! let mut events = |_: slipfork_image::BuildEvent<'_>| {};
//! let mut log = StepLog::with_events(&mut events);
//! let report = builder.build(&BuildRequest {
//!     context: "/mnt/btrfs/projects/demo".into(),
//!     dockerfile: "/mnt/btrfs/projects/demo/Dockerfile".into(),
//!     args: BuildArgs::new(),
//!     target: None,
//!     externals: std::collections::BTreeMap::new(),
//! }, &mut log)?;
//! println!("{} {}", report.key, report.rootfs.display());
//! # Ok(())
//! # }
//! ```

mod builder;
mod context;
mod copy;
mod credentials;
mod digest;
mod dockerfile;
mod error;
mod fetch;
mod identity;
mod ignore;
mod inputs;
mod layout;
mod manifest;
mod puller;
mod reference;
mod registry;
mod runtime;
mod step_log;
mod steps;
mod unpack;

use std::fmt;

pub use builder::{
    BuildReport, BuildRequest, ExternalFrom, ExternalImage, NativeImageBuilder, StageWalk,
    StepTrace, external_references, instruction_width, planned_instruction_width, stage_walk,
};
pub use context::{BuildContext, ContextEntry};
pub use credentials::RegistryCredentials;
pub use dockerfile::{
    BuildArgs, CacheMount, Command, CopySpec, DockerfileParser, ImageConfig, Instruction, Owner,
    ParsedInstruction, Port, Protocol, RunSpec, Stage, StageBase, Variables,
};
pub use error::{Error, Result};
pub use ignore::Dockerignore;
pub use inputs::build_inputs_digest;
pub use layout::ImageLayout;
pub use manifest::{Descriptor, ImageIndex, ImageManifest, Platform};
pub use puller::{OciPuller, PulledImage};
pub use reference::{ImageReference, canonical_host};
pub use registry::{Credentials, RegistryClient, RegistryScheme};
pub use runtime::{CrunStepRuntime, StepHost, Stream};
pub use step_log::{BuildEvent, StepEvent, StepLog, StepPlace, StepState, exit_detail};
pub use steps::{CacheMode, StepCache, StepLayout, StepRecord};
pub use unpack::LayerUnpacker;

/// Container architecture the caller wants a multi-arch manifest resolved for.
///
/// The composition root derives this from the host; the library never probes the environment.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Architecture {
    /// `linux/amd64`.
    X86_64,
    /// `linux/arm64`.
    Aarch64,
}

impl Architecture {
    /// The OCI platform architecture string.
    #[must_use]
    pub const fn oci(self) -> &'static str {
        match self {
            Self::X86_64 => "amd64",
            Self::Aarch64 => "arm64",
        }
    }

    /// Parses an OCI or Rust architecture name.
    #[must_use]
    pub fn from_oci(value: &str) -> Option<Self> {
        match value {
            "amd64" | "x86_64" => Some(Self::X86_64),
            "arm64" | "aarch64" => Some(Self::Aarch64),
            _ => None,
        }
    }
}

impl fmt::Display for Architecture {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.oci())
    }
}

/// Canonical image port (`specs/03-under-the-hood.md` §5).
pub trait BaseImageBuilder: Send + Sync {
    /// Pulls `reference` into a sealed read-only subvolume, or reuses the cache.
    fn pull(&self, reference: &ImageReference) -> Result<PulledImage>;
}
