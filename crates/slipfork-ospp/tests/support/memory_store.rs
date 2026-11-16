//! Filesystem-free [`SeedDumpStore`] double: proves the streamer only depends on the capability
//! port, never on a local disk, and records `discard` calls for abort-path assertions.

use std::collections::BTreeMap;
use std::io::{Cursor, Read};
use std::path::{Path, PathBuf};
use std::sync::{Mutex, MutexGuard, PoisonError};

use sha2::{Digest, Sha256};
use slipfork_ospp::{DumpStream, MAX_DUMP_WINDOW_LOG, OsppError, SeedDumpStore, VerifiedDump};

/// An in-memory [`SeedDumpStore`] that records every publish and discard.
#[derive(Debug, Default)]
pub struct MemorySeedDumpStore {
    published: Mutex<BTreeMap<String, Vec<u8>>>,
    discards: Mutex<Vec<String>>,
}

impl MemorySeedDumpStore {
    /// Whether an artifact is currently published under `sha256_compressed`.
    pub fn is_published(&self, sha256_compressed: &str) -> bool {
        self.published().contains_key(sha256_compressed)
    }

    /// Every `discard` call, in order.
    pub fn discards(&self) -> Vec<String> {
        self.discards
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }

    fn published(&self) -> MutexGuard<'_, BTreeMap<String, Vec<u8>>> {
        self.published
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
    }
}

impl SeedDumpStore for MemorySeedDumpStore {
    fn lookup(&self, _sha256_compressed: &str) -> Option<PathBuf> {
        // No filesystem: a caller cannot mount an in-memory artifact by path.
        None
    }

    fn write_from(
        &self,
        sha256_compressed: &str,
        compressed_bytes: u64,
        src: &mut dyn DumpStream,
    ) -> Result<Box<dyn VerifiedDump>, OsppError> {
        let mut archive = Vec::new();
        src.read_to_end(&mut archive)
            .map_err(|error| OsppError::SeedDumpStore {
                path: "memory".to_owned(),
                message: error.to_string(),
            })?;
        if !src.completed() {
            return Err(OsppError::SeedDumpStore {
                path: "memory".to_owned(),
                message: "stream ended before dump.end".to_owned(),
            });
        }
        if archive.len() as u64 != compressed_bytes {
            return Err(OsppError::SeedDumpStore {
                path: "memory".to_owned(),
                message: format!(
                    "stream carried {} of {compressed_bytes} advertised bytes",
                    archive.len()
                ),
            });
        }
        let actual = hex::encode(Sha256::digest(&archive));
        if actual != sha256_compressed {
            return Err(OsppError::SeedDumpStore {
                path: "memory".to_owned(),
                message: format!("digest mismatch: hashed to {actual}"),
            });
        }
        self.published()
            .insert(sha256_compressed.to_owned(), archive.clone());
        Ok(Box::new(MemoryVerifiedDump {
            sha256_compressed: sha256_compressed.to_owned(),
            archive,
        }))
    }

    fn discard(&self, sha256_compressed: &str) -> Result<(), OsppError> {
        self.discards
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push(sha256_compressed.to_owned());
        self.published().remove(sha256_compressed);
        Ok(())
    }

    fn materialize(&self, _sha256_compressed: &str) -> Result<PathBuf, OsppError> {
        Err(OsppError::SeedDumpStore {
            path: "memory".to_owned(),
            message: "an in-memory store has no filesystem to materialize into".to_owned(),
        })
    }
}

struct MemoryVerifiedDump {
    sha256_compressed: String,
    archive: Vec<u8>,
}

impl std::fmt::Debug for MemoryVerifiedDump {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("MemoryVerifiedDump")
            .field("sha256_compressed", &self.sha256_compressed)
            .field("compressed_bytes", &self.archive.len())
            .finish()
    }
}

impl VerifiedDump for MemoryVerifiedDump {
    fn sha256_compressed(&self) -> &str {
        &self.sha256_compressed
    }

    fn compressed_bytes(&self) -> u64 {
        self.archive.len() as u64
    }

    fn path(&self) -> Option<&Path> {
        None
    }

    fn open(&self) -> Result<Box<dyn Read + '_>, OsppError> {
        Ok(Box::new(Cursor::new(self.archive.clone())))
    }

    fn verify_zstd(&self) -> Result<(), OsppError> {
        let mut context = zstd::zstd_safe::DCtx::create();
        context
            .set_parameter(zstd::zstd_safe::DParameter::WindowLogMax(
                MAX_DUMP_WINDOW_LOG,
            ))
            .map_err(|error| OsppError::SeedDumpStore {
                path: "memory".to_owned(),
                message: format!("cannot bound the zstd window: {error}"),
            })?;
        let mut decoder = zstd::stream::read::Decoder::with_context(
            Cursor::new(self.archive.as_slice()),
            &mut context,
        );
        std::io::copy(&mut decoder, &mut std::io::sink()).map_err(|error| {
            OsppError::SeedDumpStore {
                path: "memory".to_owned(),
                message: format!("archive is not a valid zstd stream: {error}"),
            }
        })?;
        Ok(())
    }
}
