//! Per-step run captures: `<store>/logs/<world>/runs/<run_id>.log` and its `.idx` companion.
//!
//! Every step that actually executes work (`image` built/pulled, `setup`, and every `hook`) mints
//! one 6-character lowercase hex `run_id`, streams that step's stdout/stderr into the run capture,
//! and appends one [`RunRecord`] line to `runs/index.jsonl`. The `runs/last` and `runs/last-failed`
//! pointer files let `slipfork logs -r last|failed` resolve without scanning the catalog.
//!
//! [`crate::LogStore`] owns store-root resolution, the path vocabulary, and the tolerant catalog
//! read/write/prune operations; this module keeps the per-capture sink and the resolution rules.
//!
//! The capture is opened (and one `running` record appended) **before** the step executes, so a step
//! that dies before printing its first line still leaves a readable artifact. A step that did no
//! work (`cached`, `up to date`) is [`RunCapture::discard`]ed: its files and catalog lines go away.
//! Long-lived services keep their own `<store>/logs/<world>/<service>.log`.

use std::fs::{self, DirBuilder, File};
use std::io::{Read, Write};
use std::os::unix::fs::{DirBuilderExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

use crate::error::{Error, Result};
use crate::log_store::LogStore;
use crate::pod_log::{self, LogKind, LogSink, now_ms};

/// Width of one minted run id (`#b7f2a1`).
pub const RUN_ID_LEN: usize = 6;
/// The per-world directory holding every captured run.
pub const RUNS_DIR: &str = "runs";
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
    /// Step kind (`image`, `setup`, `hook`).
    pub kind: String,
    /// Bare node name (`backend`, `api`, `api:pre-deploy`).
    pub target: String,
    /// The command the step ran (`uv sync`), or the image id for a build.
    pub command: String,
    /// Final state (`built`, `pulled`, `ready`, `failed`).
    pub state: String,
    /// Epoch milliseconds when the step started.
    pub started_ms: u64,
    /// Epoch milliseconds when the step finished.
    pub finished_ms: Option<u64>,
    /// Wall time of the step in milliseconds.
    pub duration_ms: u64,
    /// Absolute project directory the step ran for.
    pub project: String,
    /// World id the step ran in.
    pub world: String,
}

/// One catalog read: every valid record plus how many lines were skipped.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Catalog {
    /// Valid records, newest-wins per `run_id`, oldest first.
    pub records: Vec<RunRecord>,
    /// Old-schema or damaged lines that were skipped instead of failing the read.
    pub skipped: usize,
}

/// One world's run-capture directory (`<store>/logs/<world>/runs`).
#[must_use]
pub fn runs_dir(logs_root: &Path) -> PathBuf {
    logs_root.join(RUNS_DIR)
}

/// `<runs>/<id>.log`: the canonical capture path for one run.
#[must_use]
pub fn capture_path(runs: &Path, id: &str) -> PathBuf {
    pod_log::log_path(runs, id)
}

/// `<runs>/index.jsonl`: the append-only catalog file.
#[must_use]
pub fn index_file(runs: &Path) -> PathBuf {
    runs.join(INDEX_FILE)
}

/// One open run capture: the framed `.log`, its `.idx`, and the catalog metadata.
#[derive(Debug)]
pub struct RunCapture {
    sink: LogSink,
    dir: PathBuf,
    run_id: String,
    started_ms: u64,
    kind: String,
    target: String,
    command: String,
    project: String,
    world: String,
    finished: bool,
}

impl RunCapture {
    /// Mints one unique id and opens `runs/<id>.log` plus its `.idx`.
    ///
    /// The id is 6 lowercase hex characters drawn from `/dev/urandom`, retried against the
    /// directory until no earlier capture owns it; a host without `/dev/urandom` falls back to a
    /// pid/nanosecond digest with the same retry.
    pub fn begin(
        store: &LogStore,
        project: &Path,
        kind: &str,
        target: &str,
        command: &str,
    ) -> Result<Self> {
        let dir = store.run_dir();
        DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(&dir)
            .map_err(|source| Error::io("create", &dir, source))?;
        fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700))
            .map_err(|source| Error::io("chmod", &dir, source))?;
        let run_id = mint(&dir)?;
        let sink = LogSink::open(&dir, &run_id)?;
        let started_ms = now_ms();
        // The `running` line lands before the step executes anything, so a step that errors (or is
        // killed) before its first printed line still resolves through `logs -r last`.
        if let Err(error) = crate::log_store::append_record(
            &dir,
            &RunRecord {
                run_id: run_id.clone(),
                kind: kind.to_owned(),
                target: target.to_owned(),
                command: command.to_owned(),
                state: "running".to_owned(),
                started_ms,
                finished_ms: None,
                duration_ms: 0,
                project: project.display().to_string(),
                world: store.world().as_str().to_owned(),
            },
        ) {
            // A begin that could not be catalogued must not leave an orphan capture behind.
            let _ = crate::log_store::remove_capture(&dir, &run_id);
            return Err(error);
        }
        write_pointer(&dir, LAST_FILE, &run_id)?;
        Ok(Self {
            sink,
            dir,
            run_id,
            started_ms,
            kind: kind.to_owned(),
            target: target.to_owned(),
            command: command.to_owned(),
            project: project.display().to_string(),
            world: store.world().as_str().to_owned(),
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
        let finished_ms = now_ms();
        crate::log_store::append_record(
            &self.dir,
            &RunRecord {
                run_id: self.run_id.clone(),
                kind: self.kind.clone(),
                target: self.target.clone(),
                command: self.command.clone(),
                state: state.to_owned(),
                started_ms: self.started_ms,
                finished_ms: Some(finished_ms),
                duration_ms,
                project: self.project.clone(),
                world: self.world.clone(),
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
        let _ = crate::log_store::forget_record(&dir, &run_id);
    }
}

/// Reads one world's catalog in append order, newest-wins per `run_id`.
///
/// A line written by an older schema or damaged by a partial write is skipped and counted instead
/// of failing the whole catalog, so a corrupt `index.jsonl` never bricks `slipfork logs`.
pub fn catalog(runs: &Path) -> Result<Catalog> {
    let path = index_file(runs);
    let text = match fs::read_to_string(&path) {
        Ok(text) => text,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok(Catalog::default());
        }
        Err(source) => return Err(Error::io("read", &path, source)),
    };
    let mut records: Vec<RunRecord> = Vec::new();
    let mut seen: std::collections::BTreeMap<String, usize> = std::collections::BTreeMap::new();
    let mut skipped = 0;
    for line in text.lines() {
        if line.trim().is_empty() {
            continue;
        }
        let Ok(record) = serde_json::from_str::<RunRecord>(line) else {
            skipped += 1;
            continue;
        };
        match seen.get(&record.run_id) {
            Some(&position) => records[position] = record,
            None => {
                seen.insert(record.run_id.clone(), records.len());
                records.push(record);
            }
        }
    }
    records.sort_by_key(|record| record.started_ms);
    Ok(Catalog { records, skipped })
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

pub(crate) fn read_pointer(dir: &Path, name: &str) -> Result<Option<String>> {
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

pub(crate) fn write_pointer(dir: &Path, name: &str, run_id: &str) -> Result<()> {
    let path = dir.join(name);
    let (temporary, mut file) = crate::log_store::temp_file(dir, name)?;
    file.write_all(run_id.as_bytes())
        .and_then(|()| file.write_all(b"\n"))
        .map_err(|source| Error::io("write", &temporary, source))?;
    file.flush()
        .map_err(|source| Error::io("write", &temporary, source))?;
    drop(file);
    fs::rename(&temporary, &path).map_err(|source| {
        let _ = fs::remove_file(&temporary);
        Error::io("rename", &path, source)
    })
}
