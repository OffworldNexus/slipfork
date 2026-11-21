//! Btrfs subvolumes, snapshots, world boot and reap, and the ioctls behind them.

/// Entry marker for the world store owned by this crate.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct WorldStore;

impl WorldStore {
    /// Builds the world store marker.
    #[must_use]
    pub const fn new() -> Self {
        Self
    }
}
