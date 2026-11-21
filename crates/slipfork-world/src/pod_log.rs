//! Detached-pod log capture: one append-only `<pods>/logs/<id>.log` per pod.
//!
//! Every line is written as `<epoch_ms>\tL|D\t<text>`, so replay can merge pods strictly by
//! timestamp instead of file order. The init keeps every sink open and passes the pipe read ends
//! to per-stream pump threads; a restart appends a fresh divider instead of truncating history.
//! Replay never buffers more than [`CHUNK`] bytes in the scanner.

use std::fs::{self, File, OpenOptions};
use std::io::{Read, Write};
use std::os::unix::fs::OpenOptionsExt;
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
        fs::create_dir_all(dir).map_err(|source| Error::io("create", dir, source))?;
        std::fs::set_permissions(dir, std::os::unix::fs::PermissionsExt::from_mode(0o700))
            .map_err(|source| Error::io("chmod", dir, source))?;
        let path = dir.join(format!("{id}.log"));
        let file = OpenOptions::new()
            .create(true)
            .append(true)
            .mode(0o600)
            .open(&path)
            .map_err(|source| Error::io("open", &path, source))?;
        Ok(Self { path, file })
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

#[cfg(test)]
mod tests {
    use std::io::Write;
    use std::net::Shutdown;
    use std::os::unix::fs::PermissionsExt;
    use std::os::unix::net::UnixStream;

    use super::*;

    fn scratch(name: &str) -> PathBuf {
        let dir = PathBuf::from(format!(
            "/var/tmp/slipfork-podlog-{}-{name}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).expect("create scratch dir");
        dir
    }

    #[test]
    fn capture_writes_a_private_timestamped_divider_and_lines() {
        let dir = scratch("capture");
        let mut sink = LogSink::open(&dir, "api").expect("open sink");
        assert_eq!(
            fs::metadata(sink.path())
                .expect("stat log")
                .permissions()
                .mode()
                & 0o777,
            0o600
        );
        sink.append_at(10, LogKind::Divider, "── api started ──")
            .expect("divider");
        sink.append_at(20, LogKind::Line, "hello from the pod")
            .expect("line");

        let mut reader = LogReader::open(sink.path()).expect("replay reader");
        let mut lines = Vec::new();
        reader
            .read_lines(&mut |line| lines.push(line))
            .expect("replay");
        reader.flush(&mut |line| lines.push(line));
        assert_eq!(lines.len(), 2, "{lines:?}");
        assert_eq!(lines[0].kind, LogKind::Divider);
        assert_eq!(lines[0].ts_ms, 10);
        assert_eq!(lines[1].text, "hello from the pod");
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn two_pods_interleaved_by_timestamp_merge_in_order() {
        let dir = scratch("merge");
        let mut a = LogSink::open(&dir, "api").expect("api sink");
        let mut b = LogSink::open(&dir, "worker").expect("worker sink");
        // Physical append order is deliberately not chronological.
        a.append_at(40, LogKind::Line, "a4").expect("a4");
        b.append_at(10, LogKind::Line, "b1").expect("b1");
        a.append_at(30, LogKind::Line, "a3").expect("a3");
        b.append_at(20, LogKind::Line, "b2").expect("b2");

        let mut merge = LogMerge::default();
        for (pod, path) in [
            ("api", a.path().to_path_buf()),
            ("worker", b.path().to_path_buf()),
        ] {
            let mut reader = LogReader::open(&path).expect("reader");
            reader
                .read_lines(&mut |line| merge.push(pod, line))
                .expect("read");
        }
        let merged: Vec<(String, LogLine)> = merge.flush();
        let texts: Vec<&str> = merged.iter().map(|(_, line)| line.text.as_str()).collect();
        assert_eq!(texts, ["b1", "b2", "a3", "a4"], "{merged:?}");
        assert_eq!(merged[0].0, "worker");
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn the_reorder_window_holds_recent_lines() {
        let mut merge = LogMerge::default();
        merge.push("api", LogLine::new(1_000, LogKind::Line, "old"));
        merge.push("api", LogLine::new(1_400, LogKind::Line, "new"));
        let drained: Vec<String> = merge
            .drain(1_200)
            .into_iter()
            .map(|(_, line)| line.text)
            .collect();
        assert_eq!(drained, ["old"]);
        assert_eq!(merge.flush().len(), 1);
    }

    #[test]
    fn a_socketpair_stream_is_split_into_bounded_lines() {
        let (mut writer, reader) = UnixStream::pair().expect("socketpair");
        let divider = format!("99\tD\t{DIVIDER_OPEN}worker started{DIVIDER_CLOSE}\n");
        writer.write_all(divider.as_bytes()).expect("write divider");
        writer
            .write_all(b"100\tL\tline-one\n101\tL\tline-two\n")
            .expect("write lines");
        writer.shutdown(Shutdown::Write).expect("close writer");

        let mut lines = Vec::new();
        let mut splitter = LogReader {
            file: File::open("/dev/null").expect("devnull"),
            lines: FrameLines::new(),
            path: PathBuf::from("socketpair"),
        };
        let mut read = 0_u64;
        let mut reader = reader;
        let mut chunk = [0_u8; 7];
        loop {
            let count = reader.read(&mut chunk).expect("read socketpair");
            if count == 0 {
                break;
            }
            read += count as u64;
            for raw in splitter.lines.feed(&chunk[..count], CHUNK) {
                lines.push(LogLine::parse(&raw));
            }
        }
        splitter.flush(&mut |line| lines.push(line));
        assert_eq!(
            read,
            (divider.len() + "100\tL\tline-one\n".len() + "101\tL\tline-two\n".len()) as u64,
            "every byte crosses the pipe exactly once"
        );
        assert_eq!(lines.len(), 3, "{lines:?}");
        assert_eq!(lines[0].kind, LogKind::Divider);
        assert_eq!(lines[0].ts_ms, 99);
        assert_eq!(lines[1].text, "line-one");
        assert_eq!(lines[2].ts_ms, 101);
    }
}
