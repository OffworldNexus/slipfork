/// Raw flags returned by `BTRFS_IOC_SUBVOL_GETFLAGS`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(crate) struct SubvolumeFlags(u64);

impl SubvolumeFlags {
    /// `BTRFS_SUBVOL_RDONLY`: a real kernel seal, independent of any mode bit.
    pub(crate) const READ_ONLY: u64 = 1 << 0;

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
    fn seal_round_trips_only_the_read_only_bit() {
        let plain = SubvolumeFlags::from_raw(0b10);
        assert!(!plain.is_read_only());
        assert!(plain.sealed().is_read_only());
        assert_eq!(plain.sealed().raw(), 0b11);
        assert_eq!(plain.sealed().unsealed(), plain);
    }
}
