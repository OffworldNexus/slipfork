//! `DiskSeedDumpStore`: the filesystem-backed [`SeedDumpStore`] adapter.
//!
//! All filesystem and on-disk `zstd` work lives here: `<cache_root>/dumps/<sha256>.part` is
//! streamed with an incremental SHA-256 and a byte count, refused unless `dump.end` was observed,
//! validated as a bounded-window `zstd` archive, then atomically renamed to
//! `<cache_root>/dumps/<sha256>.pgdump`. A `tmpfs` cache root is refused because `/tmp` on Linux
//! runners is RAM-backed. Discarding and re-verifying are capabilities of the returned
//! [`VerifiedDump`], so the streamer never touches the filesystem.

use std::fs::{self, File, OpenOptions};
use std::io::{self, BufReader, Read, Write};
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};

use sha2::{Digest, Sha256};

use crate::dump::{DumpStream, MAX_DUMP_WINDOW_LOG, SeedDumpStore, VerifiedDump};
use crate::error::OsppError;
use crate::hex::is_sha256_hex;

/// Suffix of a verified, digest-pinned seed dump.
pub const DUMP_SUFFIX: &str = ".pgdump";

/// Suffix of a seed dump that is still being verified.
pub const PART_SUFFIX: &str = ".part";

/// Copy buffer for the store (64 KiB).
const COPY_BUFFER: usize = 64 * 1024;

/// [`SeedDumpStore`] over `<cache_root>/dumps/<sha256>.pgdump` on a non-tmpfs filesystem.
pub struct DiskSeedDumpStore {
    dir: PathBuf,
}

impl DiskSeedDumpStore {
    /// Open (creating if needed) `<cache_root>/dumps`, rejecting a `tmpfs` cache root.
    pub fn open(cache_root: impl AsRef<Path>) -> Result<Self, OsppError> {
        let dir = cache_root.as_ref().join("dumps");
        fs::create_dir_all(&dir).map_err(|error| store_io(&dir, error))?;
        let canonical = fs::canonicalize(&dir).map_err(|error| store_io(&dir, error))?;
        let fs_type = filesystem_type(&canonical)?;
        if fs_type == "tmpfs" {
            return Err(OsppError::SeedDumpStore {
                path: canonical.display().to_string(),
                message: "seed dumps must not live on tmpfs (RAM-backed); point <cache_root> at a disk-backed filesystem such as XFS".to_owned(),
            });
        }
        Ok(Self { dir: canonical })
    }

    /// The canonical `<cache_root>/dumps` directory.
    pub fn dir(&self) -> &Path {
        &self.dir
    }
}

impl SeedDumpStore for DiskSeedDumpStore {
    fn lookup(&self, sha256_compressed: &str) -> Option<PathBuf> {
        if !is_sha256_hex(sha256_compressed) {
            return None;
        }
        let path = self.dir.join(format!("{sha256_compressed}{DUMP_SUFFIX}"));
        path.is_file().then_some(path)
    }

    fn write_from(
        &self,
        sha256_compressed: &str,
        compressed_bytes: u64,
        src: &mut dyn DumpStream,
    ) -> Result<Box<dyn VerifiedDump>, OsppError> {
        if !is_sha256_hex(sha256_compressed) {
            return Err(OsppError::SeedDumpStore {
                path: self.dir.display().to_string(),
                message: "expected a 64-character lowercase hex SHA-256".to_owned(),
            });
        }
        if compressed_bytes == 0 {
            return Err(OsppError::SeedDumpStore {
                path: self.dir.display().to_string(),
                message: "refusing to publish a zero-byte dump".to_owned(),
            });
        }
        let part = self.dir.join(format!("{sha256_compressed}{PART_SUFFIX}"));
        let published = self.dir.join(format!("{sha256_compressed}{DUMP_SUFFIX}"));
        let mut file = create_part(&part)?;
        let copy = copy_verified(&mut file, &part, src, sha256_compressed, compressed_bytes);
        if let Err(error) = copy {
            drop(file);
            let _ = fs::remove_file(&part);
            return Err(error);
        }
        if !src.completed() {
            drop(file);
            let _ = fs::remove_file(&part);
            return Err(OsppError::SeedDumpStore {
                path: part.display().to_string(),
                message: "stream ended before dump.end".to_owned(),
            });
        }
        if let Err(error) = file.sync_all() {
            drop(file);
            let _ = fs::remove_file(&part);
            return Err(store_io(&part, error));
        }
        drop(file);
        if let Err(error) = verify_archive(&part, compressed_bytes) {
            let _ = fs::remove_file(&part);
            return Err(error);
        }
        if let Err(error) = fs::rename(&part, &published) {
            let _ = fs::remove_file(&part);
            return Err(store_io(&part, error));
        }
        Ok(Box::new(DiskVerifiedDump {
            path: published,
            sha256_compressed: sha256_compressed.to_owned(),
            compressed_bytes,
        }))
    }

    fn discard(&self, sha256_compressed: &str) -> Result<(), OsppError> {
        if !is_sha256_hex(sha256_compressed) {
            return Ok(());
        }
        for path in [
            self.dir.join(format!("{sha256_compressed}{PART_SUFFIX}")),
            self.dir.join(format!("{sha256_compressed}{DUMP_SUFFIX}")),
        ] {
            match fs::remove_file(&path) {
                Ok(()) => {}
                Err(error) if error.kind() == io::ErrorKind::NotFound => {}
                Err(error) => return Err(store_io(&path, error)),
            }
        }
        Ok(())
    }
}

/// The artifact published by [`DiskSeedDumpStore`] (`<sha256>.pgdump`).
#[derive(Debug)]
struct DiskVerifiedDump {
    path: PathBuf,
    sha256_compressed: String,
    compressed_bytes: u64,
}

impl VerifiedDump for DiskVerifiedDump {
    fn sha256_compressed(&self) -> &str {
        &self.sha256_compressed
    }

    fn compressed_bytes(&self) -> u64 {
        self.compressed_bytes
    }

    fn path(&self) -> Option<&Path> {
        Some(&self.path)
    }

    fn open(&self) -> Result<Box<dyn Read + '_>, OsppError> {
        let file = File::open(&self.path).map_err(|error| store_io(&self.path, error))?;
        Ok(Box::new(BufReader::new(file)))
    }

    fn verify_zstd(&self) -> Result<(), OsppError> {
        verify_archive(&self.path, self.compressed_bytes)
    }
}

fn create_part(part: &Path) -> Result<File, OsppError> {
    if part.exists() {
        let _ = fs::remove_file(part);
    }
    OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(part)
        .map_err(|error| store_io(part, error))
}

fn copy_verified(
    file: &mut File,
    part: &Path,
    src: &mut dyn DumpStream,
    expected_sha: &str,
    expected_bytes: u64,
) -> Result<(), OsppError> {
    let mut hasher = Sha256::new();
    let mut buffer = [0u8; COPY_BUFFER];
    let mut written: u64 = 0;
    loop {
        let read = src
            .read(&mut buffer)
            .map_err(|error| OsppError::SeedDumpStore {
                path: part.display().to_string(),
                message: format!("seed-dump stream failed: {error}"),
            })?;
        if read == 0 {
            break;
        }
        written += read as u64;
        if written > expected_bytes {
            return Err(OsppError::SeedDumpStore {
                path: part.display().to_string(),
                message: format!("stream exceeded dump.begin's {expected_bytes} bytes"),
            });
        }
        hasher.update(&buffer[..read]);
        file.write_all(&buffer[..read])
            .map_err(|error| store_io(part, error))?;
    }
    if written != expected_bytes {
        return Err(OsppError::SeedDumpStore {
            path: part.display().to_string(),
            message: format!("stream carried {written} of {expected_bytes} advertised bytes"),
        });
    }
    let actual = hex::encode(hasher.finalize());
    if actual != expected_sha {
        return Err(OsppError::SeedDumpStore {
            path: part.display().to_string(),
            message: format!("digest mismatch: stream hashed to {actual}, expected {expected_sha}"),
        });
    }
    Ok(())
}

/// Verify an archive's byte count and that it decodes as zstd with a bounded window.
fn verify_archive(path: &Path, expected_bytes: u64) -> Result<(), OsppError> {
    let metadata = fs::metadata(path).map_err(|error| store_io(path, error))?;
    if metadata.len() != expected_bytes {
        return Err(OsppError::SeedDumpStore {
            path: path.display().to_string(),
            message: format!(
                "archive is {} bytes, dump.begin advertised {expected_bytes}",
                metadata.len()
            ),
        });
    }
    let file = File::open(path).map_err(|error| store_io(path, error))?;
    let mut context = zstd::zstd_safe::DCtx::create();
    context
        .set_parameter(zstd::zstd_safe::DParameter::WindowLogMax(
            MAX_DUMP_WINDOW_LOG,
        ))
        .map_err(|error| OsppError::SeedDumpStore {
            path: path.display().to_string(),
            message: format!("cannot bound the zstd window: {error}"),
        })?;
    let mut decoder = zstd::stream::read::Decoder::with_context(BufReader::new(file), &mut context);
    io::copy(&mut decoder, &mut io::sink()).map_err(|error| OsppError::SeedDumpStore {
        path: path.display().to_string(),
        message: format!("archive is not a valid zstd stream: {error}"),
    })?;
    Ok(())
}

/// Filesystem type of the mount containing `path`, from `/proc/self/mountinfo`.
fn filesystem_type(path: &Path) -> Result<String, OsppError> {
    let mountinfo =
        fs::read_to_string("/proc/self/mountinfo").map_err(|error| store_io(path, error))?;
    let mut best: Option<(usize, String)> = None;
    for line in mountinfo.lines() {
        let Some((left, right)) = line.split_once(" - ") else {
            continue;
        };
        let fields: Vec<&str> = left.split_whitespace().collect();
        if fields.len() < 5 {
            continue;
        }
        let mount_point = PathBuf::from(unescape_mount_field(fields[4]));
        if !path.starts_with(&mount_point) {
            continue;
        }
        let Some(fs_type) = right.split_whitespace().next() else {
            continue;
        };
        let mount_len = mount_point.as_os_str().len();
        if best
            .as_ref()
            .is_none_or(|(best_len, _)| mount_len >= *best_len)
        {
            best = Some((mount_len, fs_type.to_owned()));
        }
    }
    best.map(|(_, fs_type)| fs_type)
        .ok_or_else(|| OsppError::SeedDumpStore {
            path: path.display().to_string(),
            message: "cannot determine the filesystem type from /proc/self/mountinfo".to_owned(),
        })
}

/// Decode the octal escapes mountinfo uses for spaces, tabs, newlines, and backslashes.
fn unescape_mount_field(field: &str) -> String {
    let mut out = String::with_capacity(field.len());
    let mut chars = field.chars();
    while let Some(ch) = chars.next() {
        if ch != '\\' {
            out.push(ch);
            continue;
        }
        let octal: String = chars.by_ref().take(3).collect();
        match u8::from_str_radix(&octal, 8) {
            Ok(byte) => out.push(char::from(byte)),
            Err(_) => {
                out.push('\\');
                out.push_str(&octal);
            }
        }
    }
    out
}

fn store_io(path: &Path, error: io::Error) -> OsppError {
    OsppError::SeedDumpStore {
        path: path.display().to_string(),
        message: error.to_string(),
    }
}
