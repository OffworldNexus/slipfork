//! Kernel-facing Btrfs primitives: superblock probing, subvolume flags and the ioctls.

pub(crate) mod flags;
pub(crate) mod flush;
pub(crate) mod ioctls;
pub(crate) mod probe;

pub(crate) use flags::SubvolumeFlags;
pub(crate) use flush::syncfs;
pub(crate) use ioctls::{create_subvolume, destroy, fsid, set_flags, snapshot};
pub(crate) use probe::{is_btrfs, is_subvolume, subvolume_flags};
pub use probe::{is_btrfs_path, is_subvolume_path};
