//! The fixed 16-byte timestamp index beside every capture file: append, validate, rebuild.
//!
//! [`ensure_index`] is the single gate: a valid index starts at offset `0`, has strictly
//! increasing offsets, and never points past the current end of the log. Anything else (missing
//! file, `len % 16 != 0`, truncated log, empty index for a non-empty log) is rebuilt once by
//! scanning the log and replacing the `.idx` atomically; there is no silent "read from zero
//! anyway" path.

use std::fs::{self, File, OpenOptions};
use std::io::{BufRead, BufReader, Write};
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};

use crate::error::{Error, Result};

use super::{INDEX_BYTES, INDEX_ENTRY_BYTES, INDEX_LINES, index_path, log_path};

/// One fixed-size timestamp index entry: a log line starting at `byte_offset` with `ts_ms`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct IndexEntry {
    /// Milliseconds since the Unix epoch of the line at `byte_offset`.
    pub ts_ms: u64,
    /// Byte offset of that line inside the `.log`.
    pub byte_offset: u64,
}

impl IndexEntry {
    pub(super) fn encode(self) -> [u8; INDEX_ENTRY_BYTES] {
        let mut bytes = [0_u8; INDEX_ENTRY_BYTES];
        bytes[..8].copy_from_slice(&self.ts_ms.to_be_bytes());
        bytes[8..].copy_from_slice(&self.byte_offset.to_be_bytes());
        bytes
    }

    fn decode(raw: &[u8]) -> Self {
        let mut ts = [0_u8; 8];
        let mut offset = [0_u8; 8];
        ts.copy_from_slice(&raw[..8]);
        offset.copy_from_slice(&raw[8..]);
        Self {
            ts_ms: u64::from_be_bytes(ts),
            byte_offset: u64::from_be_bytes(offset),
        }
    }
}

/// Appends one fixed-size entry and flushes it; the sink tracks the liveness thresholds.
pub(super) fn append_index(file: &mut File, path: &Path, entry: IndexEntry) -> Result<()> {
    file.write_all(&entry.encode())
        .and_then(|()| file.flush())
        .map_err(|source| Error::io("write", path, source))
}

/// Reads or explicitly rebuilds one `<id>.idx` for a log of `log_len` bytes.
///
/// A valid index starts at offset `0`, has strictly increasing offsets, and never points past the
/// current end of the log. Anything else (missing file, `len % 16 != 0`, truncated log, empty
/// index for a non-empty log) is rebuilt by scanning the log once.
pub(super) fn ensure_index(index: &Path, log: &Path, log_len: u64) -> Result<Vec<IndexEntry>> {
    if let Some(entries) = read_valid_index(index, log_len) {
        return Ok(entries);
    }
    rebuild_index(index, log)?;
    read_valid_index(index, log_len).ok_or_else(|| Error::Protocol {
        message: format!(
            "rebuilt log index `{}` is still inconsistent with `{}`",
            index.display(),
            log.display()
        ),
    })
}

fn read_valid_index(index: &Path, log_len: u64) -> Option<Vec<IndexEntry>> {
    let bytes = fs::read(index).ok()?;
    if bytes.len() % INDEX_ENTRY_BYTES != 0 {
        return None;
    }
    let entries: Vec<IndexEntry> = bytes
        .chunks_exact(INDEX_ENTRY_BYTES)
        .map(IndexEntry::decode)
        .collect();
    if entries.is_empty() {
        return (log_len == 0).then_some(entries);
    }
    if entries[0].byte_offset != 0 {
        return None;
    }
    for pair in entries.windows(2) {
        if pair[1].byte_offset <= pair[0].byte_offset {
            return None;
        }
    }
    if entries.last()?.byte_offset > log_len {
        return None;
    }
    Some(entries)
}

fn rebuild_index(index: &Path, log: &Path) -> Result<()> {
    let file = File::open(log).map_err(|source| Error::io("open", log, source))?;
    let mut reader = BufReader::new(file);
    let mut entries = Vec::new();
    let mut line = Vec::new();
    let mut offset = 0_u64;
    let mut line_no = 0_u64;
    let mut indexed_at = 0_u64;
    loop {
        line.clear();
        let read = reader
            .read_until(b'\n', &mut line)
            .map_err(|source| Error::io("read", log, source))?;
        if read == 0 {
            break;
        }
        if line_no == 0
            || line_no % INDEX_LINES == 0
            || offset.saturating_sub(indexed_at) >= INDEX_BYTES
        {
            entries.push(IndexEntry {
                ts_ms: raw_ts(&line),
                byte_offset: offset,
            });
            indexed_at = offset;
        }
        offset += read as u64;
        line_no += 1;
    }
    write_index(index, &entries)
}

fn raw_ts(raw: &[u8]) -> u64 {
    let tab = raw.iter().position(|byte| *byte == b'\t').unwrap_or(0);
    std::str::from_utf8(&raw[..tab])
        .ok()
        .and_then(|text| text.trim().parse().ok())
        .unwrap_or(0)
}

fn write_index(index: &Path, entries: &[IndexEntry]) -> Result<()> {
    let mut temp = index.as_os_str().to_owned();
    temp.push(".tmp");
    let temp = PathBuf::from(temp);
    let _ = fs::remove_file(&temp);
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(&temp)
        .map_err(|source| Error::io("create", &temp, source))?;
    for entry in entries {
        file.write_all(&entry.encode())
            .map_err(|source| Error::io("write", &temp, source))?;
    }
    file.sync_all()
        .map_err(|source| Error::io("sync", &temp, source))?;
    drop(file);
    fs::rename(&temp, index).map_err(|source| Error::io("rename", index, source))
}

/// Loads (or explicitly rebuilds) one world's `.idx`; exposed for replay introspection.
pub fn load_index(logs_root: &Path, id: &str) -> Result<Vec<IndexEntry>> {
    let log = log_path(logs_root, id);
    let len = fs::metadata(&log)
        .map_err(|source| Error::io("stat", &log, source))?
        .len();
    ensure_index(&index_path(logs_root, id), &log, len)
}

/// Opens one capture file at the indexed boundary at or before `since_ms`.
pub(super) fn index_start(index: &Path, log: &Path, log_len: u64, since_ms: u64) -> Result<u64> {
    let entries = ensure_index(index, log, log_len)?;
    if entries.is_empty() {
        return Ok(0);
    }
    let position = entries.partition_point(|entry| entry.ts_ms < since_ms);
    let start = if position == 0 {
        entries[0].byte_offset
    } else {
        entries[position - 1].byte_offset
    };
    Ok(start.min(log_len))
}
