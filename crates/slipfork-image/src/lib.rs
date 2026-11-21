//! Native OCI pulls into cached read-only Btrfs subvolumes.
//!
//! [`OciPuller`] speaks the OCI Distribution API v2 directly over HTTPS: bearer-token auth,
//! multi-arch manifest-list resolution, streamed `sha256` verification, and tar+gzip/zstd
//! unpacking with whiteout semantics into a sealed read-only Btrfs subvolume. Every root and
//! registry base URL is injected by the composition root, so this crate never reads the
//! environment and never binds a socket.
//!
//! ```no_run
//! use std::sync::Arc;
//!
//! use slipfork_image::{Architecture, Credentials, ImageLayout, ImageReference, OciPuller};
//! use slipfork_world::{BtrfsEngine, StoreLayout, WorldEngine};
//!
//! # fn main() -> Result<(), Box<dyn std::error::Error>> {
//! let engine = Arc::new(BtrfsEngine::new(StoreLayout::new("/mnt/btrfs")));
//! engine.ensure_store()?;
//! let layout = ImageLayout::new(engine.layout().images_root());
//! let puller = OciPuller::new(
//!     engine,
//!     layout,
//!     Credentials::Anonymous,
//!     Architecture::X86_64,
//!     "slipfork/0.1".to_owned(),
//! );
//! let image = puller.pull(&ImageReference::parse("postgres:18")?)?;
//! println!("{} {}", image.digest, image.rootfs.display());
//! # Ok(())
//! # }
//! ```

mod digest;
mod error;
mod layout;
mod manifest;
mod puller;
mod reference;
mod registry;
mod unpack;

use std::fmt;

pub use error::{Error, Result};
pub use layout::ImageLayout;
pub use manifest::{Descriptor, ImageIndex, ImageManifest, Platform};
pub use puller::{OciPuller, PulledImage};
pub use reference::ImageReference;
pub use registry::{Credentials, RegistryClient, RegistryScheme};
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
