//! Persistent capture files: one append-only `<store>/logs/<world>/<id>.log` per pod or image.
//!
//! Every line is written as `<epoch_ms>\tO|E|L|D|S\t<text>` (`O` stdout, `E` stderr, `L` merged,
//! `D` restart divider, `S` build instruction), so replay keeps each stream while merging pods strictly by
//! timestamp instead of file order. Beside it lives the companion `<id>.idx`: fixed 16-byte
//! big-endian `(ts_ms, byte_offset)` entries written on the first line and then at least every
//! [`INDEX_LINES`] lines or [`INDEX_BYTES`] bytes. [`read_range`] binary-searches that index and
//! seeks straight to the first byte that can be inside the requested window instead of scanning a
//! large capture from offset zero.
//!
//! A missing, malformed, or stale index is rebuilt once by scanning the log and replacing the
//! `.idx` atomically; there is no silent "read from zero anyway" path. The init keeps every sink
//! open and passes the pipe read ends to per-stream pump threads; a restart appends a fresh divider
//! instead of truncating history. [`LogSink::append`] is the raw writer the image builder calls for
//! build logs, so both paths maintain the index.
//!
//! The index machinery lives in the `index` submodule and the incremental readers in `reader`;
//! this root owns the framing, the append sinks, and the canonical path helpers.

mod index;
mod reader;

pub use index::{IndexEntry, load_index};
pub use reader::{LogMerge, LogRange, LogReader, follow_range, read_range};

use std::fs::{self, DirBuilder, File, OpenOptions};
use std::io::Write;
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use crate::control::Stream;
use crate::error::{Error, Result};

use index::{append_index, ensure_index};

/// Left half of the restart divider (`── api started ──`).
pub const DIVIDER_OPEN: &str = "── ";
/// Right half of the restart divider.
pub const DIVIDER_CLOSE: &str = " ──";
/// Hard cap on any pending partial line held in memory.
pub const CHUNK: usize = 64 * 1024;
/// Reorder window for a followed stream, so two chatty pods merge chronologically.
pub const REORDER_MS: u64 = 250;
/// One index entry is exactly 16 bytes: `ts_ms: u64` then `byte_offset: u64`, big-endian.
pub const INDEX_ENTRY_BYTES: usize = 16;
/// Force an index entry after this many captured lines.
pub const INDEX_LINES: u64 = 64;
/// Force an index entry after this many captured bytes.
pub const INDEX_BYTES: u64 = 16 * 1024;

/// Whether one replayed line is pod output or a runtime divider.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LogKind {
    /// A normal stdout/stderr line.
    Line,
    /// A `── <id> started|restarted ──` divider written by the init.
    Divider,
    /// One captured build instruction row (`1/3 COPY . /app`), written by the image builder.
    Step,
}

/// The framing marker for one row: `D` divider, `S` build instruction, `O`/`E` a tagged line,
/// `L` merged output.
fn marker(kind: LogKind, stream: Stream) -> &'static str {
    match (kind, stream) {
        (LogKind::Divider, _) => "D",
        (LogKind::Step, _) => "S",
        (LogKind::Line, Stream::Stdout) => "O",
        (LogKind::Line, Stream::Stderr) => "E",
        (LogKind::Line, Stream::Combined) => "L",
    }
}

/// Decodes one framing marker into its kind and stream; anything else is legacy merged output.
fn parse_marker(marker: &[u8]) -> (LogKind, Stream) {
    match marker {
        b"D" => (LogKind::Divider, Stream::Combined),
        b"S" => (LogKind::Step, Stream::Combined),
        b"O" => (LogKind::Line, Stream::Stdout),
        b"E" => (LogKind::Line, Stream::Stderr),
        _ => (LogKind::Line, Stream::Combined),
    }
}

/// One timestamped, classified log line.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LogLine {
    /// Milliseconds since the Unix epoch, recorded when the pump wrote the line.
    pub ts_ms: u64,
    /// Line, divider, or build instruction.
    pub kind: LogKind,
    /// Which child stream produced it; `Combined` for merged or legacy captures.
    pub stream: Stream,
    /// The text without its terminator.
    pub text: String,
}

impl LogLine {
    /// Builds one merged line value; tests use this for deterministic ordering.
    #[must_use]
    pub fn new(ts_ms: u64, kind: LogKind, text: impl Into<String>) -> Self {
        Self {
            ts_ms,
            kind,
            stream: Stream::Combined,
            text: text.into(),
        }
    }

    pub(super) fn parse(raw: &[u8]) -> Self {
        let Some(tab) = raw.iter().position(|byte| *byte == b'\t') else {
            return Self::new(0, LogKind::Line, String::from_utf8_lossy(raw));
        };
        let Ok(ts) = std::str::from_utf8(&raw[..tab])
            .ok()
            .and_then(|text| text.parse::<u64>().ok())
            .ok_or(())
        else {
            return Self::new(0, LogKind::Line, String::from_utf8_lossy(raw));
        };
        let rest = &raw[tab + 1..];
        let (kind, stream, text) = match rest.iter().position(|byte| *byte == b'\t') {
            Some(sep) => {
                let (kind, stream) = parse_marker(&rest[..sep]);
                (kind, stream, &rest[sep + 1..])
            }
            None => (LogKind::Line, Stream::Combined, rest),
        };
        let text = String::from_utf8_lossy(text);
        let text = text.trim_end_matches('\r');
        Self {
            ts_ms: ts,
            kind,
            stream,
            text: text.to_owned(),
        }
    }
}

/// Milliseconds since the Unix epoch.
#[must_use]
pub fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |since| since.as_millis() as u64)
}

/// One pod's open append sink; the descriptor stays in the init across restarts.
#[derive(Debug)]
pub struct LogSink {
    path: PathBuf,
    file: File,
    index: File,
    index_path: PathBuf,
    offset: u64,
    lines_since_index: u64,
    bytes_since_index: u64,
    first_line: bool,
}

impl LogSink {
    /// Opens `<dir>/<id>.log` in append mode with mode `0600` and its `.idx` companion.
    pub fn open(dir: &Path, id: &str) -> Result<Self> {
        let path = prepare_log(dir, id)?;
        let file = open_log(&path)?;
        let index_path = index_path(dir, id);
        let log_len = file
            .metadata()
            .map_err(|source| Error::io("stat", &path, source))?
            .len();
        let entries = ensure_index(&index_path, &path, log_len)?;
        let indexed = entries.last().map_or(0, |entry| entry.byte_offset);
        let index = OpenOptions::new()
            .create(true)
            .append(true)
            .mode(0o600)
            .open(&index_path)
            .map_err(|source| Error::io("open", &index_path, source))?;
        Ok(Self {
            path,
            file,
            index,
            index_path,
            offset: log_len,
            lines_since_index: 0,
            bytes_since_index: log_len.saturating_sub(indexed),
            first_line: log_len == 0,
        })
    }

    /// The log file path.
    #[must_use]
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// The companion index path.
    #[must_use]
    pub fn index_path(&self) -> &Path {
        &self.index_path
    }

    /// Appends one timestamped line.
    pub fn append(&mut self, kind: LogKind, text: &str) -> Result<()> {
        self.append_at(now_ms(), kind, text)
    }

    /// Appends one line with an explicit timestamp; the stream is the merged capture.
    pub fn append_at(&mut self, ts_ms: u64, kind: LogKind, text: &str) -> Result<()> {
        let line = format!("{ts_ms}\t{}\t{}\n", marker(kind, Stream::Combined), text);
        self.write_line(ts_ms, line)
    }

    /// Appends one timestamped stdout/stderr line, keeping its stream in the framing.
    pub fn append_stream(&mut self, ts_ms: u64, stream: Stream, text: &str) -> Result<()> {
        let line = format!("{ts_ms}\t{}\t{}\n", marker(LogKind::Line, stream), text);
        self.write_line(ts_ms, line)
    }

    fn write_line(&mut self, ts_ms: u64, line: String) -> Result<()> {
        let offset = self.offset;
        self.file
            .write_all(line.as_bytes())
            .and_then(|()| self.file.flush())
            .map_err(|source| Error::io("write", &self.path, source))?;
        self.lines_since_index += 1;
        self.bytes_since_index += line.len() as u64;
        let indexed = (self.first_line && offset == 0)
            || self.lines_since_index >= INDEX_LINES
            || self.bytes_since_index >= INDEX_BYTES;
        if indexed {
            append_index(
                &mut self.index,
                &self.index_path,
                IndexEntry {
                    ts_ms,
                    byte_offset: offset,
                },
            )?;
            self.lines_since_index = 0;
            self.bytes_since_index = 0;
            self.first_line = false;
        }
        self.offset = offset + line.len() as u64;
        Ok(())
    }

    /// Appends the launch/restart divider the renderer draws.
    pub fn divider(&mut self, id: &str, restarted: bool) -> Result<()> {
        let verb = if restarted { "restarted" } else { "started" };
        let line = format!("{DIVIDER_OPEN}{id} {verb}{DIVIDER_CLOSE}");
        self.append(LogKind::Divider, &line)
    }
}

/// `<logs_root>/<id>.log`: the canonical capture path for one pod or image id.
#[must_use]
pub fn log_path(logs_root: &Path, id: &str) -> PathBuf {
    logs_root.join(format!("{id}.log"))
}

/// `<logs_root>/<id>.idx`: the fixed 16-byte timestamp index beside the capture file.
#[must_use]
pub fn index_path(logs_root: &Path, id: &str) -> PathBuf {
    logs_root.join(format!("{id}.idx"))
}

/// Appends raw `bytes` tagged with their stream (`O` stdout, `E` stderr, `L` merged) to
/// `<logs_root>/<id>.log` as [`LogKind::Line`] rows.
///
/// This is the shared raw writer the native image builder calls for build output, so a failed
/// build reads back through the same `<ts_ms>\t<marker>\t<text>` format and index as captured pod
/// output. The directory chain is created mode `0700` and both files mode `0600`.
pub fn append_stream(logs_root: &Path, id: &str, stream: Stream, bytes: &[u8]) -> Result<()> {
    let mut sink = LogSink::open(logs_root, id)?;
    let text = String::from_utf8_lossy(bytes);
    for line in text.split_terminator('\n') {
        sink.append_stream(now_ms(), stream, line.trim_end_matches('\r'))?;
    }
    Ok(())
}

fn prepare_log(dir: &Path, id: &str) -> Result<PathBuf> {
    // The whole `<store>/logs/<world>` chain is private, not just the leaf directory.
    DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(dir)
        .map_err(|source| Error::io("create", dir, source))?;
    fs::set_permissions(dir, fs::Permissions::from_mode(0o700))
        .map_err(|source| Error::io("chmod", dir, source))?;
    Ok(log_path(dir, id))
}

fn open_log(path: &Path) -> Result<File> {
    OpenOptions::new()
        .create(true)
        .append(true)
        .mode(0o600)
        .open(path)
        .map_err(|source| Error::io("open", path, source))
}
