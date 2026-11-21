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

#[cfg(test)]
mod tests {
    use super::SubvolumeFlags;

    #[test]
    fn read_only_is_uapi_bit_one_and_round_trips() {
        // `<linux/btrfs.h>`: `BTRFS_SUBVOL_RDONLY` is `1ULL << 1`; bit 0 is `CREATE_ASYNC`.
        assert_eq!(SubvolumeFlags::READ_ONLY, 1 << 1);
        assert_ne!(SubvolumeFlags::READ_ONLY, 1 << 0);

        let plain = SubvolumeFlags::from_raw(0);
        assert!(!plain.is_read_only());
        assert_eq!(plain.sealed().raw(), 1 << 1);
        assert!(plain.sealed().is_read_only());
        assert_eq!(plain.sealed().unsealed(), plain);

        // `GETFLAGS` on a read-only subvolume reports `0x2`; unrelated bits must survive.
        assert!(SubvolumeFlags::from_raw(0x2).is_read_only());
        let mixed = SubvolumeFlags::from_raw(0b1010);
        assert!(mixed.is_read_only());
        assert_eq!(mixed.unsealed().raw(), 0b1000);
        assert_eq!(mixed.sealed().raw(), 0b1010);
        assert!(!SubvolumeFlags::from_raw(0b1000).is_read_only());
    }
}
