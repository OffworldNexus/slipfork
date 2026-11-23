//! Per-step run captures: `<store>/logs/<world>/runs/<run_id>.log` and its `.idx` companion.
//!
//! Every step that actually executes work (`image` built/pulled, `setup`, and every `hook`) mints
//! one 6-character lowercase hex `run_id`, streams that step's stdout/stderr into the run capture,
//! and appends one [`RunRecord`] line to `runs/index.jsonl`. The `runs/last` and `runs/last-failed`
//! pointer files let `slipfork logs -r last|failed` resolve without scanning the catalog.
//!
//! A step that did no work (`cached`, `up to date`) mints no id: the capture is begun lazily on the
//! first output line and [`RunCapture::discard`] removes its files when the state turns out to be a
//! no-op. Long-lived services keep their own `<store>/logs/<world>/<service>.log`.

use std::fs::{self, DirBuilder, File, OpenOptions};
use std::io::{Read, Write};
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

use crate::error::{Error, Result};
use crate::pod_log::{self, LogKind, LogSink, now_ms};

/// Width of one minted run id (`#b7f2a1`).
pub const RUN_ID_LEN: usize = 6;
/// The append-only catalog of every recorded run.
pub const INDEX_FILE: &str = "index.jsonl";
/// Pointer to the most recently recorded run id.
pub const LAST_FILE: &str = "last";
/// Pointer to the most recent failed run id.
pub const LAST_FAILED_FILE: &str = "last-failed";

/// One line of `runs/index.jsonl`: the catalog entry for one executed step.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct RunRecord {
    /// The 6-character run id, without `#`.
    pub run_id: String,
    /// Epoch milliseconds when the step started.
    pub ts_ms: u64,
    /// Step kind (`image`, `setup`, `hook`).
    pub kind: String,
    /// Bare node name (`backend`, `api`, `api:pre-deploy`).
    pub name: String,
    /// The command the step ran (`uv sync`), or the image id for a build.
    pub command: String,
    /// Final state (`built`, `pulled`, `ready`, `failed`).
    pub state: String,
    /// Wall time of the step in milliseconds.
    pub duration_ms: u64,
}

/// One world's run-capture directory (`<store>/logs/<world>/runs`).
#[must_use]
pub fn runs_dir(logs_root: &Path) -> PathBuf {
    logs_root.join("runs")
}

/// One open run capture: the framed `.log`, its `.idx`, and the catalog metadata.
#[derive(Debug)]
pub struct RunCapture {
    sink: LogSink,
    dir: PathBuf,
    run_id: String,
    ts_ms: u64,
    kind: String,
    name: String,
    command: String,
    finished: bool,
}

impl RunCapture {
    /// Mints one unique id and opens `runs/<id>.log` plus its `.idx`.
    ///
    /// The id is 6 lowercase hex characters drawn from `/dev/urandom`, retried against the
    /// directory until no earlier capture owns it; a host without `/dev/urandom` falls back to a
    /// pid/nanosecond digest with the same retry.
    pub fn begin(logs_root: &Path, kind: &str, name: &str, command: &str) -> Result<Self> {
        let dir = runs_dir(logs_root);
        DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(&dir)
            .map_err(|source| Error::io("create", &dir, source))?;
        fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700))
            .map_err(|source| Error::io("chmod", &dir, source))?;
        let run_id = mint(&dir)?;
        let sink = LogSink::open(&dir, &run_id)?;
        Ok(Self {
            sink,
            dir,
            run_id,
            ts_ms: now_ms(),
            kind: kind.to_owned(),
            name: name.to_owned(),
            command: command.to_owned(),
            finished: false,
        })
    }

    /// The minted 6-character id.
    #[must_use]
    pub fn run_id(&self) -> &str {
        &self.run_id
    }

    /// Appends one cleaned output line to the capture.
    pub fn append(&mut self, text: &str) -> Result<()> {
        self.sink.append(LogKind::Line, text)
    }

    /// Appends the catalog line, then `last` (and `last-failed`) pointers. Idempotent.
    pub fn finish(&mut self, state: &str, duration_ms: u64) -> Result<()> {
        if self.finished {
            return Ok(());
        }
        self.finished = true;
        append_record(
            &self.dir,
            &RunRecord {
                run_id: self.run_id.clone(),
                ts_ms: self.ts_ms,
                kind: self.kind.clone(),
                name: self.name.clone(),
                command: self.command.clone(),
                state: state.to_owned(),
                duration_ms,
            },
        )?;
        write_pointer(&self.dir, LAST_FILE, &self.run_id)?;
        if state == "failed" {
            write_pointer(&self.dir, LAST_FAILED_FILE, &self.run_id)?;
        }
        Ok(())
    }

    /// Drops a capture that turned out to be a no-op (`cached`): no id, no files, no catalog line.
    pub fn discard(self) {
        let dir = self.dir;
        let run_id = self.run_id;
        drop(self.sink);
        let _ = fs::remove_file(pod_log::log_path(&dir, &run_id));
        let _ = fs::remove_file(pod_log::index_path(&dir, &run_id));
    }
}

/// Reads the whole catalog in append order; a malformed line is a real parse error.
pub fn read_index(logs_root: &Path) -> Result<Vec<RunRecord>> {
    let path = runs_dir(logs_root).join(INDEX_FILE);
    let text = match fs::read_to_string(&path) {
        Ok(text) => text,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(source) => return Err(Error::io("read", &path, source)),
    };
    let mut records = Vec::new();
    for (number, line) in text.lines().enumerate() {
        if line.trim().is_empty() {
            continue;
        }
        let record = serde_json::from_str::<RunRecord>(line).map_err(|error| Error::Protocol {
            message: format!(
                "malformed run index `{}` line {}: {error}",
                path.display(),
                number + 1
            ),
        })?;
        records.push(record);
    }
    Ok(records)
}

/// Resolves `c91e04`, `#c91e04`, `last`, or `failed` to one recorded run id.
pub fn resolve_run(logs_root: &Path, spec: &str) -> Result<String> {
    let dir = runs_dir(logs_root);
    let trimmed = spec.trim();
    let key = trimmed.strip_prefix('#').unwrap_or(trimmed);
    let run_id = match key {
        "" => {
            return Err(Error::Protocol {
                message: format!(
                    "a run id is required (`<id>`, `#<id>`, `last`, or `failed`) in `{}`",
                    dir.display()
                ),
            });
        }
        "last" => read_pointer(&dir, LAST_FILE)?.ok_or_else(|| Error::Protocol {
            message: format!("no runs recorded in `{}`", dir.display()),
        })?,
        "failed" => read_pointer(&dir, LAST_FAILED_FILE)?.ok_or_else(|| Error::Protocol {
            message: format!("no failed runs recorded in `{}`", dir.display()),
        })?,
        other => other.to_owned(),
    };
    if !pod_log::log_path(&dir, &run_id).is_file() {
        return Err(Error::Protocol {
            message: format!("no recorded run `{run_id}` in `{}`", dir.display()),
        });
    }
    Ok(run_id)
}

fn mint(dir: &Path) -> Result<String> {
    for attempt in 0..32_u64 {
        let mut raw = [0_u8; 3];
        let random = File::open("/dev/urandom")
            .and_then(|mut file| file.read_exact(&mut raw))
            .is_ok();
        if !random {
            let nanos = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map_or(0, |since| since.subsec_nanos() as u64);
            let mixed = nanos
                .wrapping_mul(0x9E37_79B9_7F4A_7C15)
                .wrapping_add(u64::from(std::process::id()))
                ^ attempt.wrapping_mul(0x2545_F491_4F6C_DD1D);
            raw = [
                (mixed >> 16) as u8,
                (mixed >> 8) as u8,
                (mixed ^ (mixed >> 24)) as u8,
            ];
        }
        let run_id: String = raw.iter().map(|byte| format!("{byte:02x}")).collect();
        debug_assert_eq!(run_id.len(), RUN_ID_LEN);
        if !pod_log::log_path(dir, &run_id).exists() {
            return Ok(run_id);
        }
    }
    Err(Error::Protocol {
        message: format!("could not mint a unique run id in `{}`", dir.display()),
    })
}

fn append_record(dir: &Path, record: &RunRecord) -> Result<()> {
    let path = dir.join(INDEX_FILE);
    let line = serde_json::to_string(record).map_err(|error| Error::Protocol {
        message: format!("serialize run record: {error}"),
    })?;
    let mut file = OpenOptions::new()
        .create(true)
        .append(true)
        .mode(0o600)
        .open(&path)
        .map_err(|source| Error::io("open", &path, source))?;
    file.write_all(line.as_bytes())
        .and_then(|()| file.write_all(b"\n"))
        .and_then(|()| file.flush())
        .map_err(|source| Error::io("write", &path, source))
}

fn read_pointer(dir: &Path, name: &str) -> Result<Option<String>> {
    let path = dir.join(name);
    match fs::read_to_string(&path) {
        Ok(text) => {
            let value = text.trim();
            Ok((!value.is_empty()).then(|| value.to_owned()))
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(source) => Err(Error::io("read", &path, source)),
    }
}

fn write_pointer(dir: &Path, name: &str, run_id: &str) -> Result<()> {
    let path = dir.join(name);
    let temporary = path.with_extension("tmp");
    let mut file = OpenOptions::new()
        .create(true)
        .truncate(true)
        .write(true)
        .mode(0o600)
        .open(&temporary)
        .map_err(|source| Error::io("create", &temporary, source))?;
    file.write_all(run_id.as_bytes())
        .and_then(|()| file.write_all(b"\n"))
        .map_err(|source| Error::io("write", &temporary, source))?;
    fs::rename(&temporary, &path).map_err(|source| Error::io("rename", &path, source))
}
