//! `#![forbid(unsafe_code)]` ELF/libc ABI inspection (`ADR-0009` §2.3, §4.1).
//!
//! Parses the ELF header (`e_machine` + `PT_INTERP` program header) and the C library's version
//! symbols (`GLIBC_2.*` in `libc.so.6`, or the `ld-musl-*.so.1` version string). The result gates
//! `--host-caches` fail-closed: an architecture or libc mismatch never mounts host toolchains.

#![forbid(unsafe_code)]

use std::path::{Path, PathBuf};

use slipfork_world::rootfs::BaseImageError;
use slipfork_world::rootfs::{ElfAbi, LibcFamily};

use crate::rootfs::resolve_guest_path;

const ELF_MAGIC: &[u8; 4] = b"\x7fELF";
const PT_INTERP: u32 = 3;
const EM_X86_64: u16 = 62;
const EM_AARCH64: u16 = 183;

/// Concrete ELF/libc inspector: pure-Rust ELF header + libc version-symbol parsing.
#[derive(Debug, Default, Clone, Copy)]
pub struct ElfProbe;

impl ElfProbe {
    /// Probe `binary` (a host path under `rootfs`; `rootfs` is `/` for a host binary).
    pub fn probe(&self, rootfs: &Path, binary: &Path) -> Result<ElfAbi, BaseImageError> {
        let bytes = std::fs::read(binary).map_err(|error| BaseImageError::ElfInspection {
            path: binary.display().to_string(),
            reason: error.to_string(),
        })?;
        let (arch, interp) = parse_elf(&bytes).map_err(|reason| BaseImageError::ElfInspection {
            path: binary.display().to_string(),
            reason,
        })?;
        let libc = if interp.contains("ld-musl") {
            LibcFamily::Musl
        } else if interp.contains("ld-linux") {
            LibcFamily::Glibc
        } else {
            return Err(BaseImageError::ElfInspection {
                path: binary.display().to_string(),
                reason: format!(
                    "PT_INTERP '{interp}' is neither glibc (ld-linux-*) nor musl (ld-musl-*)"
                ),
            });
        };
        let libc_version = match libc {
            LibcFamily::Glibc => glibc_version(rootfs, &arch, &interp)?,
            LibcFamily::Musl => musl_version(rootfs, &interp)?,
        };
        Ok(ElfAbi {
            arch,
            libc,
            libc_version,
        })
    }
}

/// `true` when host caches cannot be mounted into a world with `world` ABI (`ADR-0009` §2.3).
pub(crate) fn host_cache_abi_mismatch(host: &ElfAbi, world: &ElfAbi) -> bool {
    if host.arch != world.arch || host.libc != world.libc {
        return true;
    }
    match host.libc {
        LibcFamily::Glibc => version_tuple(&host.libc_version) > version_tuple(&world.libc_version),
        LibcFamily::Musl => version_tuple(&host.libc_version) != version_tuple(&world.libc_version),
    }
}

/// Base image the ABI fix line suggests for a host libc family.
pub(crate) fn suggested_base(host: &ElfAbi) -> &'static str {
    match host.libc {
        LibcFamily::Glibc => "debian:trixie-slim",
        LibcFamily::Musl => "alpine:3.21",
    }
}

fn version_tuple(version: &str) -> (u64, u64) {
    let mut parts = version
        .split('.')
        .map(|part| part.parse::<u64>().unwrap_or(0));
    (parts.next().unwrap_or(0), parts.next().unwrap_or(0))
}

/// Return `(arch, PT_INTERP path)` from an ELF image.
fn parse_elf(bytes: &[u8]) -> Result<(String, String), String> {
    if bytes.len() < 64 || &bytes[..4] != ELF_MAGIC {
        return Err("not an ELF binary".to_owned());
    }
    let class = bytes[4];
    let little_endian = match bytes[5] {
        1 => true,
        2 => false,
        other => return Err(format!("unknown ELF data encoding {other}")),
    };
    let read_u16 = |offset: usize| -> u16 {
        let slice = [bytes[offset], bytes[offset + 1]];
        if little_endian {
            u16::from_le_bytes(slice)
        } else {
            u16::from_be_bytes(slice)
        }
    };
    let read_u32 = |offset: usize| -> u32 {
        let slice = [
            bytes[offset],
            bytes[offset + 1],
            bytes[offset + 2],
            bytes[offset + 3],
        ];
        if little_endian {
            u32::from_le_bytes(slice)
        } else {
            u32::from_be_bytes(slice)
        }
    };
    let read_u64 = |offset: usize| -> u64 {
        let mut slice = [0u8; 8];
        slice.copy_from_slice(&bytes[offset..offset + 8]);
        if little_endian {
            u64::from_le_bytes(slice)
        } else {
            u64::from_be_bytes(slice)
        }
    };

    let arch = match read_u16(18) {
        EM_X86_64 => "x86_64",
        EM_AARCH64 => "aarch64",
        machine => return Err(format!("unsupported ELF e_machine {machine}")),
    };
    let (phoff, phentsize, phnum) = match class {
        2 => (
            read_u64(32) as usize,
            read_u16(54) as usize,
            read_u16(56) as usize,
        ),
        1 => (
            read_u32(28) as usize,
            read_u16(42) as usize,
            read_u16(44) as usize,
        ),
        other => return Err(format!("unsupported ELF class {other}")),
    };

    for index in 0..phnum {
        let base = phoff + index * phentsize;
        let end = base + phentsize.max(if class == 2 { 56 } else { 32 });
        if end > bytes.len() {
            return Err("program header table exceeds the file".to_owned());
        }
        if read_u32(base) != PT_INTERP {
            continue;
        }
        let (offset, size) = if class == 2 {
            (read_u64(base + 8) as usize, read_u64(base + 32) as usize)
        } else {
            (read_u32(base + 4) as usize, read_u32(base + 16) as usize)
        };
        let end = offset.saturating_add(size);
        if end > bytes.len() {
            return Err("PT_INTERP segment exceeds the file".to_owned());
        }
        let interp = bytes[offset..end]
            .split(|byte| *byte == 0)
            .next()
            .unwrap_or_default();
        let interp = String::from_utf8_lossy(interp).into_owned();
        return Ok((arch.to_owned(), interp));
    }
    Err("no PT_INTERP program header (statically linked binary)".to_owned())
}

fn glibc_version(rootfs: &Path, arch: &str, interp: &str) -> Result<String, BaseImageError> {
    let triplet = match arch {
        "x86_64" => "x86_64-linux-gnu",
        "aarch64" => "aarch64-linux-gnu",
        other => {
            return Err(BaseImageError::ElfInspection {
                path: interp.to_owned(),
                reason: format!("no glibc triple for architecture '{other}'"),
            });
        }
    };
    let interp_dir = Path::new(interp)
        .parent()
        .and_then(Path::to_str)
        .unwrap_or("/lib");
    let mut candidates: Vec<PathBuf> = Vec::new();
    for dir in [
        format!("/lib/{triplet}"),
        format!("/usr/lib/{triplet}"),
        "/lib64".to_owned(),
        "/lib".to_owned(),
        "/usr/lib64".to_owned(),
        "/usr/lib".to_owned(),
        interp_dir.to_owned(),
    ] {
        candidates.push(PathBuf::from(dir).join("libc.so.6"));
    }
    for candidate in &candidates {
        let Some(path) = resolve_guest_path(rootfs, &candidate.to_string_lossy()) else {
            continue;
        };
        let Ok(bytes) = std::fs::read(&path) else {
            continue;
        };
        if &bytes[..4.min(bytes.len())] != ELF_MAGIC {
            continue;
        }
        if let Some(version) = max_glibc_symbol(&bytes) {
            return Ok(version);
        }
    }
    Err(BaseImageError::ElfInspection {
        path: format!("{}/libc.so.6", rootfs.display()),
        reason: format!("no GLIBC_2.* version symbol found (interpreter {interp})"),
    })
}

/// Highest `GLIBC_2.<minor>` symbol version in `bytes`, rendered `2.<minor>`.
fn max_glibc_symbol(bytes: &[u8]) -> Option<String> {
    const PREFIX: &[u8] = b"GLIBC_2.";
    let mut max: Option<u64> = None;
    let mut index = 0;
    while index + PREFIX.len() < bytes.len() {
        if &bytes[index..index + PREFIX.len()] != PREFIX {
            index += 1;
            continue;
        }
        let mut cursor = index + PREFIX.len();
        let mut minor = 0u64;
        let mut digits = 0;
        while let Some(byte) = bytes.get(cursor) {
            if !byte.is_ascii_digit() {
                break;
            }
            minor = minor * 10 + u64::from(byte - b'0');
            digits += 1;
            cursor += 1;
        }
        if digits > 0 {
            max = Some(max.map_or(minor, |current| current.max(minor)));
        }
        index = cursor;
    }
    max.map(|minor| format!("2.{minor}"))
}

fn musl_version(rootfs: &Path, interp: &str) -> Result<String, BaseImageError> {
    let path = resolve_guest_path(rootfs, interp).ok_or_else(|| BaseImageError::ElfInspection {
        path: interp.to_owned(),
        reason: "musl interpreter not found inside the rootfs".to_owned(),
    })?;
    let bytes = std::fs::read(&path).map_err(|error| BaseImageError::ElfInspection {
        path: path.display().to_string(),
        reason: error.to_string(),
    })?;
    let version = scan_dotted_version(&bytes).ok_or_else(|| BaseImageError::ElfInspection {
        path: path.display().to_string(),
        reason: "no musl version string found".to_owned(),
    })?;
    Ok(version)
}

/// First NUL-delimited `major.minor.patch` token, rendered `major.minor`.
fn scan_dotted_version(bytes: &[u8]) -> Option<String> {
    let mut index = 0;
    while index < bytes.len() {
        if index > 0 && bytes[index - 1] != 0 {
            index += 1;
            continue;
        }
        let mut cursor = index;
        let mut parts = Vec::new();
        loop {
            let mut value = 0u64;
            let mut digits = 0;
            while let Some(byte) = bytes.get(cursor) {
                if !byte.is_ascii_digit() {
                    break;
                }
                value = value * 10 + u64::from(byte - b'0');
                digits += 1;
                cursor += 1;
            }
            if digits == 0 {
                break;
            }
            parts.push(value);
            match bytes.get(cursor) {
                Some(b'.') => cursor += 1,
                Some(0) => break,
                _ => {
                    parts.clear();
                    break;
                }
            }
        }
        if parts.len() >= 2 && parts.len() <= 3 && bytes.get(cursor) == Some(&0) {
            return Some(format!("{}.{}", parts[0], parts[1]));
        }
        index += 1;
    }
    None
}

#[cfg(test)]
mod tests {
    use super::{
        ElfAbi, LibcFamily, host_cache_abi_mismatch, max_glibc_symbol, scan_dotted_version,
    };

    #[test]
    fn glibc_symbol_scan_takes_the_highest_minor() {
        let bytes = b"\0GLIBC_2.4\0GLIBC_2.41\0GLIBC_2.34\0GLIBC_2.2.5\0";
        assert_eq!(max_glibc_symbol(bytes).as_deref(), Some("2.41"));
    }

    #[test]
    fn musl_version_scan_normalizes_to_major_minor() {
        assert_eq!(
            scan_dotted_version(b"\0path\0\x001.2.5\0junk").as_deref(),
            Some("1.2")
        );
        assert_eq!(scan_dotted_version(b"no version"), None);
    }

    #[test]
    fn host_cache_mismatch_follows_the_abi_rules() {
        let host = ElfAbi {
            arch: "x86_64".to_owned(),
            libc: LibcFamily::Glibc,
            libc_version: "2.41".to_owned(),
        };
        let same = ElfAbi {
            arch: "x86_64".to_owned(),
            libc: LibcFamily::Glibc,
            libc_version: "2.41".to_owned(),
        };
        let newer_world = ElfAbi {
            libc_version: "2.42".to_owned(),
            ..same.clone()
        };
        let older_world = ElfAbi {
            libc_version: "2.36".to_owned(),
            ..same.clone()
        };
        let musl = ElfAbi {
            libc: LibcFamily::Musl,
            libc_version: "1.2".to_owned(),
            ..same.clone()
        };
        assert!(!host_cache_abi_mismatch(&host, &same));
        assert!(!host_cache_abi_mismatch(&host, &newer_world));
        assert!(host_cache_abi_mismatch(&host, &older_world));
        assert!(host_cache_abi_mismatch(&host, &musl));
    }
}
