//! Out-of-band sealed dump reader (`specs/05-ospp-v1.md` §8).
//!
//! The client only ever opens with RFC 9180 stateful `SetupBaseR`
//! (`info = "ospp-v1-dump"`): 65-byte uncompressed `enc` header, then
//! `4-byte big-endian plaintext length || ctx.Open(aad, ciphertext)` frames, and
//! a mandatory final `len = 0` sealed chunk. A stream that ends before that EOF
//! frame fails closed. Chunks stream through a small window — the whole dump is
//! never buffered.

use std::io::{self, Read};

use crate::error::OsppError;
use crate::keys::{AEAD_TAG_BYTES, KEM_ENCODED_KEY_BYTES, ReceiverContext, SubscriptionKey};

/// Largest accepted plaintext chunk in a sealed dump stream (1 MiB).
pub const MAX_DUMP_CHUNK: usize = 1024 * 1024;

/// Streaming decoder over an out-of-band HTTPS body.
pub struct DumpReader<R: Read> {
    source: R,
    context: ReceiverContext,
    aad: String,
    pending: Vec<u8>,
    offset: usize,
    finished: bool,
}

impl<R: Read> DumpReader<R> {
    /// Read the `enc` header and bind the stream to `<subscription_id>:<resource>`.
    pub fn open(
        mut source: R,
        key: &SubscriptionKey,
        subscription_id: &str,
        resource: &str,
    ) -> Result<Self, OsppError> {
        let mut header = [0u8; KEM_ENCODED_KEY_BYTES];
        source.read_exact(&mut header).map_err(|error| {
            if error.kind() == io::ErrorKind::UnexpectedEof {
                OsppError::TruncatedDump
            } else {
                OsppError::from(error)
            }
        })?;
        let (context, aad) = key.dump_context(&header, subscription_id, resource)?;
        Ok(Self {
            source,
            context,
            aad,
            pending: Vec::new(),
            offset: 0,
            finished: false,
        })
    }

    fn truncated(message: &str) -> io::Error {
        io::Error::new(io::ErrorKind::UnexpectedEof, message.to_owned())
    }

    fn next_chunk(&mut self) -> io::Result<Option<Vec<u8>>> {
        if self.finished {
            return Ok(None);
        }
        let mut length = [0u8; 4];
        self.source
            .read_exact(&mut length)
            .map_err(|error| match error.kind() {
                io::ErrorKind::UnexpectedEof => {
                    Self::truncated("sealed dump ended without a len = 0 EOF frame")
                }
                _ => error,
            })?;
        let expected = u32::from_be_bytes(length) as usize;
        if expected > MAX_DUMP_CHUNK {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "sealed dump chunk exceeds the 1 MiB bound",
            ));
        }
        let mut sealed = vec![0u8; expected + AEAD_TAG_BYTES];
        self.source
            .read_exact(&mut sealed)
            .map_err(|error| match error.kind() {
                io::ErrorKind::UnexpectedEof => {
                    Self::truncated("sealed dump frame ended mid-ciphertext")
                }
                _ => error,
            })?;
        let plaintext = self
            .context
            .open(&sealed, self.aad.as_bytes())
            .map_err(|_| {
                io::Error::new(
                    io::ErrorKind::InvalidData,
                    "sealed dump frame failed to open",
                )
            })?;
        if plaintext.len() != expected {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "sealed dump frame length does not match its plaintext",
            ));
        }
        if expected == 0 {
            self.finished = true;
            return Ok(None);
        }
        Ok(Some(plaintext))
    }
}

impl<R: Read> Read for DumpReader<R> {
    fn read(&mut self, out: &mut [u8]) -> io::Result<usize> {
        if out.is_empty() {
            return Ok(0);
        }
        while self.offset >= self.pending.len() {
            match self.next_chunk()? {
                Some(chunk) => {
                    self.pending = chunk;
                    self.offset = 0;
                }
                None => return Ok(0),
            }
        }
        let available = &self.pending[self.offset..];
        let len = available.len().min(out.len());
        out[..len].copy_from_slice(&available[..len]);
        self.offset += len;
        Ok(len)
    }
}
