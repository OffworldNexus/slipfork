/// Raw flags returned by `BTRFS_IOC_SUBVOL_GETFLAGS`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(crate) struct SubvolumeFlags(u64);

impl SubvolumeFlags {
    /// `BTRFS_SUBVOL_RDONLY` from `<linux/btrfs.h>`: bit **1**, a real kernel seal.
    /// Bit 0 is `BTRFS_SUBVOL_CREATE_ASYNC`, which `SETFLAGS` rejects with `EOPNOTSUPP`.
    pub(crate) const READ_ONLY: u64 = 1 << 1;

    pub(crate) const fn from_raw(raw: u64) -> Self {
        Self(raw)
    }

    pub(crate) const fn raw(self) -> u64 {
        self.0
    }

    pub(crate) const fn is_read_only(self) -> bool {
        self.0 & Self::READ_ONLY != 0
    }

    pub(crate) const fn sealed(self) -> Self {
        Self(self.0 | Self::READ_ONLY)
    }

    pub(crate) const fn unsealed(self) -> Self {
        Self(self.0 & !Self::READ_ONLY)
    }
}
