//! Persistent capture files: one append-only `<store>/logs/<world>/<id>.log` per pod or image.
//!
//! Every line is written as `<epoch_ms>\tL|D\t<text>`, so replay can merge pods strictly by
//! timestamp instead of file order. The init keeps every sink open and passes the pipe read ends
//! to per-stream pump threads; a restart appends a fresh divider instead of truncating history.
//! Replay never buffers more than [`CHUNK`] bytes in the scanner. [`append`] is the raw writer the
//! image builder will call for build logs.

use std::fs::{self, DirBuilder, File, OpenOptions};
use std::io::{Read, Write};
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
}

impl LogSink {
    /// Opens `<dir>/<id>.log` in append mode with mode `0600`.
    pub fn open(dir: &Path, id: &str) -> Result<Self> {
        let path = prepare_log(dir, id)?;
        Ok(Self {
            file: open_log(&path)?,
            path,
        })
    }

    /// The log file path.
    #[must_use]
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Appends one timestamped line.
    pub fn append(&mut self, kind: LogKind, text: &str) -> Result<()> {
        self.append_at(now_ms(), kind, text)
    }

    /// Appends one line with an explicit timestamp.
    pub fn append_at(&mut self, ts_ms: u64, kind: LogKind, text: &str) -> Result<()> {
        let line = format!("{ts_ms}\t{}\t{}\n", kind.marker(), text);
        self.file
            .write_all(line.as_bytes())
            .and_then(|()| self.file.flush())
            .map_err(|source| Error::io("write", &self.path, source))
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

/// Appends raw `bytes` to `<logs_root>/<id>.log` as framed [`LogKind::Line`] rows.
///
/// This is the shared raw writer the next slice's image builder will call for build output, so a
/// failed build reads back through the same `<ts_ms>\tL\t<text>` format as captured pod output.
/// The directory chain is created mode `0700` and the file mode `0600`.
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
        let file = File::open(path).map_err(|source| Error::io("open", path, source))?;
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
