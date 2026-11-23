//! Persistent capture files: one append-only `<store>/logs/<world>/<id>.log` per pod or image.
//!
//! Every line is written as `<epoch_ms>\tL|D\t<text>`, so replay can merge pods strictly by
//! timestamp instead of file order. Beside it lives the companion `<id>.idx`: fixed 16-byte
//! big-endian `(ts_ms, byte_offset)` entries written on the first line and then at least every
//! [`INDEX_LINES`] lines or [`INDEX_BYTES`] bytes. [`read_range`] binary-searches that index and
//! seeks straight to the first byte that can be inside the requested window instead of scanning a
//! large capture from offset zero.
//!
//! A missing, malformed, or stale index is rebuilt once by scanning the log and replacing the
//! `.idx` atomically; there is no silent "read from zero anyway" path. The init keeps every sink
//! open and passes the pipe read ends to per-stream pump threads; a restart appends a fresh divider
//! instead of truncating history. [`append`] is the raw writer the image builder calls for build
//! logs, so both paths maintain the index.

use std::fs::{self, DirBuilder, File, OpenOptions};
use std::io::{BufRead, BufReader, Read, Seek, SeekFrom, Write};
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use crate::control::FrameLines;
use crate::error::{Error, Result};

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

/// One fixed-size timestamp index entry: a log line starting at `byte_offset` with `ts_ms`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct IndexEntry {
    /// Milliseconds since the Unix epoch of the line at `byte_offset`.
    pub ts_ms: u64,
    /// Byte offset of that line inside the `.log`.
    pub byte_offset: u64,
}

impl IndexEntry {
    fn encode(self) -> [u8; INDEX_ENTRY_BYTES] {
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

/// Whether one replayed line is pod output or a runtime divider.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LogKind {
    /// A normal stdout/stderr line.
    Line,
    /// A `── <id> started|restarted ──` divider written by the init.
    Divider,
}

impl LogKind {
    const fn marker(self) -> &'static str {
        match self {
            Self::Line => "L",
            Self::Divider => "D",
        }
    }

    fn parse(marker: &[u8]) -> Self {
        if marker == b"D" {
            Self::Divider
        } else {
            Self::Line
        }
    }
}

/// One timestamped, classified log line.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LogLine {
    /// Milliseconds since the Unix epoch, recorded when the pump wrote the line.
    pub ts_ms: u64,
    /// Line or divider.
    pub kind: LogKind,
    /// The text without its terminator.
    pub text: String,
}

impl LogLine {
    /// Builds one line value; tests use this for deterministic ordering.
    #[must_use]
    pub fn new(ts_ms: u64, kind: LogKind, text: impl Into<String>) -> Self {
        Self {
            ts_ms,
            kind,
            text: text.into(),
        }
    }

    fn parse(raw: &[u8]) -> Self {
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
        let (kind, text) = match rest.iter().position(|byte| *byte == b'\t') {
            Some(sep) => {
                let kind = LogKind::parse(&rest[..sep]);
                (kind, &rest[sep + 1..])
            }
            None => (LogKind::Line, rest),
        };
        let text = String::from_utf8_lossy(text);
        let text = text.trim_end_matches('\r');
        Self::new(ts, kind, text)
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

    /// Appends one line with an explicit timestamp.
    pub fn append_at(&mut self, ts_ms: u64, kind: LogKind, text: &str) -> Result<()> {
        let line = format!("{ts_ms}\t{}\t{}\n", kind.marker(), text);
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

/// Appends raw `bytes` to `<logs_root>/<id>.log` as framed [`LogKind::Line`] rows.
///
/// This is the shared raw writer the native image builder calls for build output, so a failed
/// build reads back through the same `<ts_ms>\tL\t<text>` format and index as captured pod output.
/// The directory chain is created mode `0700` and both files mode `0600`.
pub fn append(logs_root: &Path, id: &str, bytes: &[u8]) -> Result<()> {
    let mut sink = LogSink::open(logs_root, id)?;
    let text = String::from_utf8_lossy(bytes);
    for line in text.split_terminator('\n') {
        sink.append(LogKind::Line, line.trim_end_matches('\r'))?;
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

fn append_index(file: &mut File, path: &Path, entry: IndexEntry) -> Result<()> {
    file.write_all(&entry.encode())
        .and_then(|()| file.flush())
        .map_err(|source| Error::io("write", path, source))
}

/// Reads or explicitly rebuilds one `<id>.idx` for a log of `log_len` bytes.
///
/// A valid index starts at offset `0`, has strictly increasing offsets, and never points past the
/// current end of the log. Anything else (missing file, `len % 16 != 0`, truncated log, empty
/// index for a non-empty log) is rebuilt by scanning the log once.
fn ensure_index(index: &Path, log: &Path, log_len: u64) -> Result<Vec<IndexEntry>> {
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

/// Reads one append-only log file incrementally, splitting bytes into timestamped lines.
#[derive(Debug)]
pub struct LogReader {
    file: File,
    lines: FrameLines,
    path: PathBuf,
}

impl LogReader {
    /// Opens `path` read-only at its start.
    pub fn open(path: &Path) -> Result<Self> {
        Self::open_at(path, 0)
    }

    fn open_at(path: &Path, offset: u64) -> Result<Self> {
        let mut file = File::open(path).map_err(|source| Error::io("open", path, source))?;
        if offset > 0 {
            file.seek(SeekFrom::Start(offset))
                .map_err(|source| Error::io("seek", path, source))?;
        }
        Ok(Self {
            file,
            lines: FrameLines::new(),
            path: path.to_path_buf(),
        })
    }

    /// Reads whatever is available and emits completed lines.
    ///
    /// Returns the number of bytes consumed; `0` means the file has no new bytes yet.
    pub fn read_lines(&mut self, on_line: &mut dyn FnMut(LogLine)) -> Result<usize> {
        let mut chunk = [0_u8; CHUNK];
        let read = self
            .file
            .read(&mut chunk)
            .map_err(|source| Error::io("read", &self.path, source))?;
        if read > 0 {
            for raw in self.lines.feed(&chunk[..read], CHUNK) {
                on_line(LogLine::parse(&raw));
            }
        }
        Ok(read)
    }

    /// Emits one trailing partial line, if any.
    pub fn flush(&mut self, on_line: &mut dyn FnMut(LogLine)) {
        if let Some(raw) = self.lines.take_partial() {
            on_line(LogLine::parse(&raw));
        }
    }
}

/// One indexed replay window over a capture file.
///
/// Construction seeks to the last index boundary at or before `since_ms`, so a follow-less replay
/// and a live follower both start at the window instead of byte zero.
#[derive(Debug)]
pub struct LogRange {
    reader: LogReader,
    since_ms: Option<u64>,
    until_ms: Option<u64>,
    start_offset: u64,
    done: bool,
}

impl LogRange {
    /// Reads available lines strictly inside `[since_ms, until_ms]`.
    ///
    /// Returns the number of bytes read from the underlying file (including skipped pre-window
    /// lines); `0` means the file has no new bytes yet or the window is exhausted.
    pub fn read_lines(&mut self, on_line: &mut dyn FnMut(LogLine)) -> Result<usize> {
        if self.done {
            return Ok(0);
        }
        let since = self.since_ms;
        let until = self.until_ms;
        let mut past_window = false;
        let read = self.reader.read_lines(&mut |line| {
            if since.is_some_and(|since| line.ts_ms < since) {
                return;
            }
            if until.is_some_and(|until| line.ts_ms > until) {
                past_window = true;
                return;
            }
            on_line(line);
        })?;
        if past_window {
            self.done = true;
        }
        Ok(read)
    }

    /// Emits one trailing partial line when it is inside the window.
    pub fn flush(&mut self, on_line: &mut dyn FnMut(LogLine)) {
        if self.done {
            return;
        }
        let since = self.since_ms;
        let until = self.until_ms;
        self.reader.flush(&mut |line| {
            if since.is_some_and(|since| line.ts_ms < since) {
                return;
            }
            if until.is_some_and(|until| line.ts_ms > until) {
                return;
            }
            on_line(line);
        });
    }

    /// The byte offset this window started reading from (the binary-search boundary).
    #[must_use]
    pub fn start_offset(&self) -> u64 {
        self.start_offset
    }
}

/// Opens one capture file at the indexed boundary at or before `since_ms`.
pub fn read_range(
    logs_root: &Path,
    id: &str,
    since_ms: Option<u64>,
    until_ms: Option<u64>,
) -> Result<LogRange> {
    let path = log_path(logs_root, id);
    let log_len = fs::metadata(&path)
        .map_err(|source| Error::io("stat", &path, source))?
        .len();
    let start_offset = match since_ms {
        None => 0,
        Some(since) => index_start(&index_path(logs_root, id), &path, log_len, since)?,
    };
    Ok(LogRange {
        reader: LogReader::open_at(&path, start_offset)?,
        since_ms,
        until_ms,
        start_offset,
        done: false,
    })
}

/// Follow variant: starts at `since_ms` (or byte zero) and keeps reading as the log grows.
pub fn follow_range(logs_root: &Path, id: &str, since_ms: Option<u64>) -> Result<LogRange> {
    read_range(logs_root, id, since_ms, None)
}

fn index_start(index: &Path, log: &Path, log_len: u64, since_ms: u64) -> Result<u64> {
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

/// Chronological merge buffer shared by replay and follow.
///
/// Lines within the reorder window are held; `drain` releases everything older than the cutoff in
/// timestamp order, and `flush` releases the rest. Ties break by arrival sequence so the order is
/// stable across identical timestamps.
#[derive(Debug, Default)]
pub struct LogMerge {
    pending: Vec<(u64, u64, String, LogLine)>,
    sequence: u64,
}

impl LogMerge {
    /// Queues one line for `pod`.
    pub fn push(&mut self, pod: &str, line: LogLine) {
        self.sequence += 1;
        self.pending
            .push((line.ts_ms, self.sequence, pod.to_owned(), line));
    }

    /// Releases every line whose timestamp is at or before `cutoff_ms`, oldest first.
    pub fn drain(&mut self, cutoff_ms: u64) -> Vec<(String, LogLine)> {
        let (ready, waiting): (Vec<_>, Vec<_>) = std::mem::take(&mut self.pending)
            .into_iter()
            .partition(|(ts, _, _, _)| *ts <= cutoff_ms);
        self.pending = waiting;
        self.sorted(ready)
    }

    /// Releases every queued line, oldest first.
    pub fn flush(&mut self) -> Vec<(String, LogLine)> {
        let pending = std::mem::take(&mut self.pending);
        self.sorted(pending)
    }

    fn sorted(&self, mut rows: Vec<(u64, u64, String, LogLine)>) -> Vec<(String, LogLine)> {
        rows.sort_by_key(|(ts, sequence, _, _)| (*ts, *sequence));
        rows.into_iter()
            .map(|(_, _, pod, line)| (pod, line))
            .collect()
    }
}
