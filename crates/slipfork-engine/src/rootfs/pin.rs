//! Compile-time pins for the static `crun` binary staged into every world (`ADR-0009` §1.1).
//!
//! The pin is metadata, not a download manager: the Composition Root provisions the release asset
//! under `<state_dir>/cache/runtime/<file_name>` (mode `0755`), and every build verifies the bytes
//! against [`CrunPin::sha256`] before they become `/ .slipfork/bin/crun`. There is no fallback to
//! a distro `crun` package and no floating "latest" download.

#![forbid(unsafe_code)]

use crate::rootfs::BaseImageError;

/// One verified static `crun` release asset.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CrunPin {
    /// Upstream `crun` release version.
    pub version: &'static str,
    /// Linux architecture (`x86_64`).
    pub arch: &'static str,
    /// Cache file name: `crun-<version>-<arch>-<sha256 prefix>.bin`.
    pub file_name: &'static str,
    /// Full lowercase hex SHA-256 of the asset.
    pub sha256: &'static str,
    /// Upstream release asset URL the pin was measured from.
    pub release_url: &'static str,
}

/// The only measured pin (`docs/design/world-base-image.md` §7.1).
pub const CRUN_PIN_X86_64: CrunPin = CrunPin {
    version: "1.20",
    arch: "x86_64",
    file_name: "crun-1.20-x86_64-ff8407185ec4.bin",
    sha256: "ff8407185ec474d7c60f697032b7b1fc1fb028fbd0bf13c7d5da9a7d9a479e38",
    release_url: "https://github.com/containers/crun/releases/download/1.20/crun-1.20-linux-amd64-disable-systemd",
};

/// Every architecture with a verified pin.
pub const PINNED_ARCHES: &str = "x86_64";

/// Look up the pin for `arch`, failing closed when no asset was measured.
pub fn crun_pin(arch: &str) -> Result<&'static CrunPin, BaseImageError> {
    match arch {
        "x86_64" => Ok(&CRUN_PIN_X86_64),
        other => Err(BaseImageError::UnsupportedArch {
            arch: other.to_owned(),
            pinned: PINNED_ARCHES,
        }),
    }
}

/// `docker buildx --platform` value for a pinned architecture.
pub fn buildx_platform(arch: &str) -> Result<&'static str, BaseImageError> {
    match crun_pin(arch)?.arch {
        "x86_64" => Ok("linux/amd64"),
        "aarch64" => Ok("linux/arm64"),
        other => Err(BaseImageError::UnsupportedArch {
            arch: other.to_owned(),
            pinned: PINNED_ARCHES,
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::{CRUN_PIN_X86_64, buildx_platform, crun_pin};

    #[test]
    fn pin_name_carries_the_sha256_prefix() {
        assert!(CRUN_PIN_X86_64.file_name.starts_with("crun-1.20-x86_64-"));
        assert_eq!(&CRUN_PIN_X86_64.sha256[..12], "ff8407185ec4");
        assert!(
            CRUN_PIN_X86_64
                .file_name
                .contains(&CRUN_PIN_X86_64.sha256[..12])
        );
    }

    #[test]
    fn unknown_architectures_fail_closed() {
        assert!(crun_pin("x86_64").is_ok());
        assert!(crun_pin("riscv64").is_err());
        assert_eq!(buildx_platform("x86_64").expect("pinned"), "linux/amd64");
    }
}
