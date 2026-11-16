//! Streaming PostgreSQL seed dumps: the [`SeedDumpStore`] capability port and the frame streamer.
//!
//! [`DumpStreamer`] drives the `dump.begin -> binary frames -> dump.end` range over the
//! authenticated session (`ADR-0008` §2). Every binary WebSocket frame is capped at
//! [`MAX_DUMP_FRAME`] and the SHA-256/byte count of the compressed archive are checked while
//! streaming. This module performs **no filesystem access**: publishing, verifying, opening, and
//! discarding the artifact are all [`SeedDumpStore`] capabilities, so a store backed by object
//! storage, a container volume, or memory implements the same contract as
//! [`crate::DiskSeedDumpStore`].

use std::fmt;
use std::io::{self, Read};
use std::path::Path;
use std::path::PathBuf;

use slipfork_world::ProjectSlug;

use crate::client::{MAX_DUMP_FRAME, OsppClient, OsppConnection, scope_label};
use crate::error::OsppError;
use crate::hex::is_sha256_hex;
use crate::protocol::{
    CAPABILITY_DUMP_BEGIN, CAPABILITY_DUMP_END, CapabilityRequest, ServerFrame, capability_error,
    validate_key, validate_mode,
};
use crate::resources::PgDumpFormat;

/// Upper bound on the decoder's zstd window (`2^23` = 8 MiB), so a hostile archive cannot force
/// an unbounded allocation while a store validates it.
pub const MAX_DUMP_WINDOW_LOG: u32 = 23;

/// The source `SeedDumpStore::write_from` consumes: the `dump.begin..dump.end` byte stream plus an
/// explicit completion signal.
///
/// A store must refuse to publish unless [`Self::completed`] is `true`, which only happens after a
/// matching `dump.end` frame was observed. A truncated connection can therefore never be mistaken
/// for a complete archive.
pub trait DumpStream: Read {
    /// Whether the reader observed `dump.end` with matching metadata.
    fn completed(&self) -> bool;
}

/// A verified, content-addressed seed-dump artifact returned by a [`SeedDumpStore`].
///
/// The store owns publication and verification; callers only read metadata or ask the artifact to
/// open itself. `path` is `None` for stores without a filesystem (an in-memory or remote cache).
pub trait VerifiedDump: Send + Sync + fmt::Debug {
    /// SHA-256 of the compressed archive (64 lowercase hex characters).
    fn sha256_compressed(&self) -> &str;

    /// Compressed archive size in bytes.
    fn compressed_bytes(&self) -> u64;

    /// Filesystem path of the published archive, if the store has one.
    fn path(&self) -> Option<&Path>;

    /// Open the verified archive for reading.
    fn open(&self) -> Result<Box<dyn Read + '_>, OsppError>;

    /// Re-validate the archive as a zstd stream with a bounded window.
    fn verify_zstd(&self) -> Result<(), OsppError>;
}

/// Content-addressed cache for verified seed dumps (`ADR-0008` §2).
pub trait SeedDumpStore: Send + Sync {
    /// Return the published dump path for `sha256_compressed`, if it is already cached.
    fn lookup(&self, sha256_compressed: &str) -> Option<PathBuf>;

    /// Persist, verify, and publish `src` under `sha256_compressed`.
    ///
    /// `compressed_bytes` is the size `dump.begin` advertised. Implementations must not publish an
    /// artifact unless the stream completed (`src.completed()`), the byte count matches, and the
    /// SHA-256 matches; anything else is discarded before this method returns an error.
    fn write_from(
        &self,
        sha256_compressed: &str,
        compressed_bytes: u64,
        src: &mut dyn DumpStream,
    ) -> Result<Box<dyn VerifiedDump>, OsppError>;

    /// Remove any partial or published artifact for `sha256_compressed` (no-op when absent).
    fn discard(&self, sha256_compressed: &str) -> Result<(), OsppError>;

    /// Materialize the verified archive as the uncompressed `pg_dump -Fc` file and return its path.
    ///
    /// The decoder is bounded by [`MAX_DUMP_WINDOW_LOG`], so a hostile archive cannot force an
    /// unbounded allocation. The `postgres:<v>` container mounts the uncompressed file at
    /// `/dumps/<sha256>.pgcustom`, so it never needs a `zstd` binary of its own (`ADR-0008` §2).
    fn materialize(&self, sha256_compressed: &str) -> Result<PathBuf, OsppError>;
}

/// Streams one digest-pinned PostgreSQL seed dump into a [`SeedDumpStore`].
pub struct DumpStreamer<'a> {
    client: &'a OsppClient,
}

impl<'a> DumpStreamer<'a> {
    /// Drive dumps over `client`'s authenticated session.
    pub const fn new(client: &'a OsppClient) -> Self {
        Self { client }
    }

    /// Fetch `db_id` for `project`/`mode`, persist it through `store`, and return the verified
    /// artifact.
    ///
    /// The caller decides *whether* a dump is wanted (`DumpPolicy` / `ResourcePlan`) and may use
    /// [`SeedDumpStore::lookup`] with the offer's digest to avoid a transfer; this method always
    /// performs the live `dump.begin` exchange and never infers a policy.
    pub fn fetch(
        &self,
        project: &ProjectSlug,
        mode: &str,
        db_id: &str,
        store: &dyn SeedDumpStore,
    ) -> Result<Box<dyn VerifiedDump>, OsppError> {
        let capability = CAPABILITY_DUMP_BEGIN;
        let scope = scope_label(project, mode);
        validate_mode(mode).map_err(|message| capability_error(capability, &scope, message))?;
        validate_key("db_id", db_id)
            .map_err(|message| capability_error(capability, &scope, message))?;

        let request_id = self.client.request_id();
        let mut session = self.client.session();
        session.send_json(
            capability,
            &CapabilityRequest::DumpBegin {
                request_id,
                project: project.as_str(),
                mode,
                db_id,
            },
        )?;
        let metadata = match session.read_frame(capability)? {
            ServerFrame::DumpBegin {
                request_id: seen,
                db_id: seen_db,
                sha256_compressed,
                compressed_bytes,
                format: PgDumpFormat::PgCustomZstd,
            } => DumpMetadata {
                request_id: seen,
                db_id: seen_db,
                sha256_compressed,
                compressed_bytes,
            },
            other => return Err(other.into_refusal(capability, &scope, request_id)),
        };
        if metadata.request_id != request_id {
            return Err(reject_metadata(
                &mut session,
                capability,
                &scope,
                format!(
                    "dump.begin answered request {}, expected {request_id}",
                    metadata.request_id
                ),
            ));
        }
        if metadata.db_id != db_id {
            return Err(reject_metadata(
                &mut session,
                capability,
                &scope,
                "server answered for a different db_id",
            ));
        }
        if !is_sha256_hex(&metadata.sha256_compressed) {
            return Err(reject_metadata(
                &mut session,
                capability,
                &scope,
                "dump.begin digest is not 64 lowercase hex characters",
            ));
        }
        if metadata.compressed_bytes == 0 {
            return Err(reject_metadata(
                &mut session,
                capability,
                &scope,
                "dump.begin advertises a zero-byte archive",
            ));
        }

        let mut reader = DumpFrameReader::new(
            &mut session,
            request_id,
            &metadata.sha256_compressed,
            metadata.compressed_bytes,
        );
        let written = store.write_from(
            &metadata.sha256_compressed,
            metadata.compressed_bytes,
            &mut reader,
        );
        let completed = reader.completed();
        drop(reader);
        if !completed {
            // A dump abandoned between `dump.begin` and `dump.end` leaves unread binary frames on
            // the session; it must be re-established rather than silently resynchronized.
            session.poison("seed dump aborted before dump.end");
        }
        drop(session);

        if !completed {
            let discard = store.discard(&metadata.sha256_compressed);
            return Err(OsppError::SeedDump {
                capability,
                scope,
                db_id: db_id.to_owned(),
                message: match (written, discard) {
                    (Err(error), _) => error.to_string(),
                    (Ok(_), Ok(())) => "seed-dump store published before dump.end".to_owned(),
                    (Ok(_), Err(error)) => {
                        format!(
                            "seed-dump store published before dump.end; discard failed: {error}"
                        )
                    }
                },
            });
        }

        let dump = written.map_err(|error| OsppError::SeedDump {
            capability,
            scope: scope.clone(),
            db_id: db_id.to_owned(),
            message: error.to_string(),
        })?;
        if dump.sha256_compressed() != metadata.sha256_compressed
            || dump.compressed_bytes() != metadata.compressed_bytes
        {
            let discard = store.discard(&metadata.sha256_compressed);
            return Err(OsppError::SeedDump {
                capability,
                scope,
                db_id: db_id.to_owned(),
                message: match discard {
                    Ok(()) => "seed-dump store returned a different artifact".to_owned(),
                    Err(error) => {
                        format!(
                            "seed-dump store returned a different artifact; discard failed: {error}"
                        )
                    }
                },
            });
        }
        Ok(dump)
    }
}

struct DumpMetadata {
    request_id: u32,
    db_id: String,
    sha256_compressed: String,
    compressed_bytes: u64,
}

/// Reject `dump.begin` metadata after the server has started the range; the unread binary frames
/// make the session unusable, so it is poisoned before the typed error is returned.
fn reject_metadata(
    session: &mut OsppConnection,
    capability: &'static str,
    scope: &str,
    message: impl Into<String>,
) -> OsppError {
    session.poison("dump.begin metadata rejected before the binary range");
    capability_error(capability, scope, message)
}

/// `Read` adapter over the `dump.begin..dump.end` binary range.
struct DumpFrameReader<'a> {
    session: &'a mut OsppConnection,
    request_id: u32,
    sha256_compressed: String,
    expected_bytes: u64,
    received: u64,
    pending: Vec<u8>,
    offset: usize,
    finished: bool,
}

impl<'a> DumpFrameReader<'a> {
    fn new(
        session: &'a mut OsppConnection,
        request_id: u32,
        sha256_compressed: &str,
        expected_bytes: u64,
    ) -> Self {
        Self {
            session,
            request_id,
            sha256_compressed: sha256_compressed.to_owned(),
            expected_bytes,
            received: 0,
            pending: Vec::new(),
            offset: 0,
            finished: false,
        }
    }
}

impl DumpStream for DumpFrameReader<'_> {
    fn completed(&self) -> bool {
        self.finished
    }
}

impl Read for DumpFrameReader<'_> {
    fn read(&mut self, out: &mut [u8]) -> io::Result<usize> {
        if out.is_empty() {
            return Ok(0);
        }
        loop {
            if self.offset < self.pending.len() {
                let available = &self.pending[self.offset..];
                let len = available.len().min(out.len());
                out[..len].copy_from_slice(&available[..len]);
                self.offset += len;
                return Ok(len);
            }
            if self.finished {
                return Ok(0);
            }
            match self.session.read_inbound(CAPABILITY_DUMP_END) {
                Ok(crate::client::Inbound::Binary(bytes)) => {
                    if bytes.is_empty() {
                        continue;
                    }
                    if bytes.len() > MAX_DUMP_FRAME {
                        return Err(invalid_data(format!(
                            "binary frame of {} bytes exceeded the 64 KiB cap",
                            bytes.len()
                        )));
                    }
                    self.received += bytes.len() as u64;
                    if self.received > self.expected_bytes {
                        return Err(invalid_data(
                            "received more bytes than dump.begin advertised",
                        ));
                    }
                    self.pending = bytes;
                    self.offset = 0;
                }
                Ok(crate::client::Inbound::Text(text)) => {
                    let frame = serde_json::from_str::<ServerFrame>(&text).map_err(|error| {
                        invalid_data(format!("malformed dump.end frame: {error}"))
                    })?;
                    match frame {
                        ServerFrame::DumpEnd {
                            request_id,
                            sha256_compressed,
                            compressed_bytes,
                        } if request_id == self.request_id => {
                            if sha256_compressed != self.sha256_compressed
                                || compressed_bytes != self.expected_bytes
                            {
                                return Err(invalid_data(
                                    "dump.end metadata differs from dump.begin",
                                ));
                            }
                            if self.received != self.expected_bytes {
                                return Err(invalid_data(format!(
                                    "truncated dump: received {} of {} bytes",
                                    self.received, self.expected_bytes
                                )));
                            }
                            self.finished = true;
                            return Ok(0);
                        }
                        ServerFrame::DumpEnd { .. } => {
                            return Err(invalid_data("dump.end does not match this request"));
                        }
                        other => {
                            return Err(invalid_data(format!(
                                "`{}` frame inside the seed-dump binary range",
                                other.kind()
                            )));
                        }
                    }
                }
                Ok(crate::client::Inbound::Closed) => {
                    return Err(io::Error::new(
                        io::ErrorKind::UnexpectedEof,
                        "connection closed before dump.end",
                    ));
                }
                Err(error) => return Err(io::Error::other(error.to_string())),
            }
        }
    }
}

fn invalid_data(message: impl Into<String>) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message.into())
}
