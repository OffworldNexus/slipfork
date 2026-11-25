//! Incremental readers over capture files: line splitting, indexed windows, chronological merge.

use std::fs::{self, File};
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};

use crate::control::FrameLines;
use crate::error::{Error, Result};

use super::index::index_start;
use super::{CHUNK, LogLine, index_path, log_path};

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
