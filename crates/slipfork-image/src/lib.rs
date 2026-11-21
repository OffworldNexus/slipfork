//! Native OCI pulls and `Dockerfile` builds into cached Btrfs step subvolumes.

/// Entry marker for the base image cache owned by this crate.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct ImageStore;

impl ImageStore {
    /// Builds the image store marker.
    #[must_use]
    pub const fn new() -> Self {
        Self
    }
}
