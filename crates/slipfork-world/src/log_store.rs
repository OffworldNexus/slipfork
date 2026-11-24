//! One owner for the store root and every world run-capture path and catalog.
//!
//! Every caller injects the mount table, the project directory, `$XDG_DATA_HOME`, and the selected
//! mount's writability; this module never reads the environment or probes the host. A store root
//! selected on the Btrfs filesystem root (`/`) or on a read-only mount resolves under
//! `<data_home>/slipfork/store` instead of producing `/.slipfork-store`, and `<store>/logs/<world>`
//! is derived through [`StoreLayout`].
//!
//! Scheduler workers (one per DAG node) and the daemon both mutate the same catalog, so every
//! mutation holds one exclusive `flock` on `runs/index.jsonl`, re-reads inside it, and publishes
//! rewrites through a per-call unique sibling.
//!
//! Retention prunes only terminal successes (`built`, `pulled`, `ready`). A `failed` run, a crashed
//! run still marked `running`, and any unknown state are never counted or swept, so the evidence an
//! operator needs outlives every teardown.

use std::fs::{self, DirBuilder, File, OpenOptions};
use std::io::Write;
use std::os::unix::fs::{DirBuilderExt, MetadataExt, OpenOptionsExt};
use std::os::unix::io::AsRawFd;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use crate::error::{Error, Result};
use crate::layout::{STORE_DIR, StoreLayout, WorldName};
use crate::pod_log;
use crate::run_log::{self, INDEX_FILE, LAST_FAILED_FILE, LAST_FILE, RunNote, RunRecord};

/// How many terminal-successful run captures one world keeps after a teardown.
///
/// A `failed` record, a crashed record still marked `running`, and any unknown state are never
/// counted or pruned: the artifact an operator needs after a failed boot must outlive every
/// `up`/`down`/`fork`.
pub const RETAIN_RUNS: usize = 20;

/// Whether a run finished successfully, i.e. is safe for retention to prune.
fn is_terminal_success(state: &str) -> bool {
    matches!(state, "built" | "pulled" | "ready")
}

/// The one store path and run-capture catalog for a single world.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LogStore {
    root: PathBuf,
    world: WorldName,
}

impl LogStore {
    /// Resolves the store root for `project` from an injected mount table.
    ///
    /// The longest mount containing `project` wins (a nested Btrfs mount beats its parent). A store
    /// on the filesystem root or on a mount the caller cannot write resolves under
    /// `<data_home>/slipfork/store`, so it never yields `/.slipfork-store`. An empty project or an
    /// empty mount table also resolves under `data_home`.
    #[must_use]
    pub fn root_for(
        mounts: &[PathBuf],
        project: &Path,
        data_home: &Path,
        mount_writable: bool,
    ) -> PathBuf {
        let mut best: Option<(&Path, usize)> = None;
        for mount in mounts {
            if project.starts_with(mount) {
                let depth = mount.components().count();
                if best.is_none_or(|(_, best_depth)| depth >= best_depth) {
                    best = Some((mount.as_path(), depth));
                }
            }
        }
        match best {
            Some((mount, _)) if mount_writable && mount != Path::new("/") => mount.join(STORE_DIR),
            _ => data_home.join("slipfork").join("store"),
        }
    }

    /// Builds the store at an already-resolved `root` for one world.
    #[must_use]
    pub fn at(root: impl Into<PathBuf>, world: WorldName) -> Self {
        Self {
            root: root.into(),
            world,
        }
    }

    /// The resolved `<mount>/.slipfork-store` (or `<data_home>/slipfork/store`).
    #[must_use]
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// The world whose captures this store addresses.
    #[must_use]
    pub fn world(&self) -> &WorldName {
        &self.world
    }

    /// `<store>/logs/<world>`: this world's capture directory.
    #[must_use]
    pub fn world_logs(&self) -> PathBuf {
        StoreLayout::at(self.root.clone()).world_logs(&self.world)
    }

    /// `<store>/logs/<world>/runs`: the catalog and per-step captures.
    #[must_use]
    pub fn run_dir(&self) -> PathBuf {
        run_log::runs_dir(&self.world_logs())
    }

    /// `<store>/logs/<world>/runs/<id>.log`: one run's canonical capture file.
    #[must_use]
    pub fn capture(&self, id: &str) -> PathBuf {
        run_log::capture_path(&self.run_dir(), id)
    }

    /// `<store>/logs/<world>/runs/index.jsonl`: the append-only catalog.
    #[must_use]
    pub fn index(&self) -> PathBuf {
        run_log::index_file(&self.run_dir())
    }

    /// Appends one catalog line for a finished run.
    pub fn record(&self, record: &RunRecord) -> Result<()> {
        append_record(&self.run_dir(), record)
    }

    /// Re-publishes one recorded run with the instruction it actually executed.
    ///
    /// The catalog keeps the newest line per `run_id`, so annotating after a run finishes leaves
    /// exactly one terminal record. Only the fields `note` carries are patched; an unknown run id
    /// is an explicit error instead of a silent no-op.
    pub fn annotate_run(&self, run_id: &str, note: &RunNote) -> Result<()> {
        let mut record = self
            .records()?
            .records
            .into_iter()
            .find(|record| record.run_id == run_id)
            .ok_or_else(|| Error::Protocol {
                message: format!("no recorded run `{run_id}` to annotate"),
            })?;
        if let Some(phase) = &note.phase {
            record.phase.clone_from(phase);
        }
        if let Some(instruction) = &note.instruction {
            record.instruction.clone_from(instruction);
        }
        if let Some(source) = &note.source {
            record.source.clone_from(source);
        }
        if note.exit_code.is_some() {
            record.exit_code = note.exit_code;
        }
        append_record(&self.run_dir(), &record)
    }

    /// Every recorded run, newest-wins per `run_id`, oldest first, with skipped-line count.
    ///
    /// Lines written by an older schema or damaged by a partial write are counted and skipped, so
    /// one bad line can never brick `slipfork logs`.
    pub fn records(&self) -> Result<run_log::Catalog> {
        run_log::catalog(&self.run_dir())
    }

    /// The newest recorded run whose `target` matches, whatever its kind.
    ///
    /// A hook's target is its phase-qualified name (`api:pre-deploy`) and an image build's target is
    /// its image id, so `slipfork logs api` reaches the failed build that never opened a pod.
    pub fn resolve_target(&self, target: &str) -> Result<Option<RunRecord>> {
        Ok(self
            .records()?
            .records
            .into_iter()
            .filter(|record| record.target == target)
            .max_by_key(|record| record.started_ms))
    }

    /// Resolves `<id>`, `#<id>`, `last`, or `failed` to one recorded run id for this world.
    ///
    /// Callers never re-derive `<store>/logs/<world>/runs`; this is the one spec-to-id entry point.
    pub fn resolve_run(&self, spec: &str) -> Result<String> {
        run_log::resolve_run(&self.world_logs(), spec)
    }

    /// Prunes all but the newest `newest` terminal successes, returning how many were removed.
    ///
    /// Only `built`, `pulled`, and `ready` records are eligible. A `failed` record, a crashed run
    /// still marked `running`, and any unknown state keep their `.log`/`.idx` forever: the evidence
    /// an operator needs after a teardown is never swept.
    pub fn retain(&self, newest: usize) -> Result<usize> {
        let dir = self.run_dir();
        if !dir.is_dir() {
            return Ok(0);
        }
        let mut lock = CatalogLock::acquire(&dir)?;
        // Re-read inside the lock: a step may have appended its terminal line since the caller
        // looked, and pruning a stale view would drop a live capture.
        let mut records = run_log::catalog(&dir)?.records;
        records.sort_by_key(|record| record.started_ms);
        let successes = records
            .iter()
            .filter(|record| is_terminal_success(&record.state))
            .count();
        let prune_budget = successes.saturating_sub(newest);
        let mut dropped_so_far = 0;
        let mut kept = Vec::with_capacity(records.len());
        let mut pruned = 0;
        for record in records {
            let keep = !is_terminal_success(&record.state) || dropped_so_far >= prune_budget;
            if keep {
                kept.push(record);
            } else {
                dropped_so_far += 1;
                remove_capture(&dir, &record.run_id)?;
                pruned += 1;
            }
        }
        if pruned > 0 {
            lock.replace(&kept)?;
            refresh_pointers(&dir, &kept)?;
        }
        Ok(pruned)
    }
}

/// One exclusive advisory lock on a world's `index.jsonl`.
///
/// The identity check makes a prune's atomic rename safe: a lock taken on a just-unlinked inode is
/// released and retried against the file now at the path, so no append is ever written into a
/// catalog nobody reads.
struct CatalogLock {
    file: File,
    path: PathBuf,
}

impl CatalogLock {
    fn acquire(dir: &Path) -> Result<Self> {
        DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(dir)
            .map_err(|source| Error::io("create", dir, source))?;
        let path = dir.join(INDEX_FILE);
        for _ in 0..32 {
            let file = OpenOptions::new()
                .read(true)
                .append(true)
                .create(true)
                .mode(0o600)
                .open(&path)
                .map_err(|source| Error::io("open", &path, source))?;
            lock_exclusive(&file).map_err(|source| Error::io("lock", &path, source))?;
            let locked = file
                .metadata()
                .map_err(|source| Error::io("stat", &path, source))?;
            let current = match fs::metadata(&path) {
                Ok(metadata) => metadata,
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
                Err(source) => return Err(Error::io("stat", &path, source)),
            };
            if locked.ino() == current.ino() && locked.dev() == current.dev() {
                return Ok(Self { file, path });
            }
        }
        Err(Error::Protocol {
            message: format!("could not lock the run catalog at `{}`", path.display()),
        })
    }

    /// Appends one already-serialized line under the held lock.
    fn append(&mut self, line: &str) -> Result<()> {
        self.file
            .write_all(line.as_bytes())
            .and_then(|()| self.file.write_all(b"\n"))
            .and_then(|()| self.file.flush())
            .map_err(|source| Error::io("write", &self.path, source))
    }

    /// Replaces the catalog with `records` through a per-call unique sibling, then renames it over
    /// the locked path.
    fn replace(&mut self, records: &[RunRecord]) -> Result<()> {
        let dir = self.path.parent().ok_or_else(|| Error::Protocol {
            message: format!("`{}` has no parent directory", self.path.display()),
        })?;
        let (temporary, mut file) = temp_file(dir, INDEX_FILE)?;
        for record in records {
            let line = serde_json::to_string(record).map_err(|error| Error::Protocol {
                message: format!("serialize run record: {error}"),
            })?;
            file.write_all(line.as_bytes())
                .and_then(|()| file.write_all(b"\n"))
                .map_err(|source| Error::io("write", &temporary, source))?;
        }
        file.flush()
            .map_err(|source| Error::io("write", &temporary, source))?;
        drop(file);
        fs::rename(&temporary, &self.path).map_err(|source| {
            let _ = fs::remove_file(&temporary);
            Error::io("rename", &self.path, source)
        })
    }
}

/// Blocks until one exclusive advisory lock is held on `file`.
///
/// `libc::flock` is the only MSRV-1.85 way to take it; the wrapper is the single `unsafe` in this
/// module and only passes a live descriptor the process already owns.
#[allow(unsafe_code)]
fn lock_exclusive(file: &File) -> std::io::Result<()> {
    loop {
        // SAFETY: `fd` is owned by `file` and stays open for the borrow; `flock` only inspects it.
        let locked = unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX) };
        if locked == 0 {
            return Ok(());
        }
        let error = std::io::Error::last_os_error();
        if error.kind() != std::io::ErrorKind::Interrupted {
            return Err(error);
        }
    }
}

/// Creates a per-call unique `0600` temporary sibling named after `name`.
///
/// The pid, a nanosecond stamp, and a create-new retry counter keep worker threads and the daemon
/// from ever sharing a temp path; the library deliberately holds no process-global counter.
pub(crate) fn temp_file(dir: &Path, name: &str) -> Result<(PathBuf, File)> {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |since| since.as_nanos());
    for attempt in 0..64_u32 {
        let path = dir.join(format!(
            ".{name}.{}.{nanos}.{attempt}.tmp",
            std::process::id()
        ));
        match OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&path)
        {
            Ok(file) => return Ok((path, file)),
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
            Err(source) => return Err(Error::io("create", &path, source)),
        }
    }
    Err(Error::Protocol {
        message: format!(
            "could not create a temporary catalog beside `{}`",
            dir.join(name).display()
        ),
    })
}

/// Appends one `RunRecord` as a single `index.jsonl` line under the catalog lock.
pub(crate) fn append_record(dir: &Path, record: &RunRecord) -> Result<()> {
    let line = serde_json::to_string(record).map_err(|error| Error::Protocol {
        message: format!("serialize run record: {error}"),
    })?;
    CatalogLock::acquire(dir)?.append(&line)
}

/// Drops every catalog line for `run_id` and refreshes the pointers it owned.
///
/// A capture that turns out to be a no-op (`cached`, `up to date`) must leave no trace in
/// `slipfork logs --runs`, so [`run_log::RunCapture::discard`] calls this after removing its files.
pub(crate) fn forget_record(dir: &Path, run_id: &str) -> Result<()> {
    let mut lock = CatalogLock::acquire(dir)?;
    let kept: Vec<RunRecord> = run_log::catalog(dir)?
        .records
        .into_iter()
        .filter(|record| record.run_id != run_id)
        .collect();
    lock.replace(&kept)?;
    refresh_pointers(dir, &kept)
}

/// Removes one run's `.log` and `.idx`.
pub(crate) fn remove_capture(dir: &Path, run_id: &str) -> Result<()> {
    for path in [
        pod_log::log_path(dir, run_id),
        pod_log::index_path(dir, run_id),
    ] {
        match fs::remove_file(&path) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(source) => return Err(Error::io("remove", &path, source)),
        }
    }
    Ok(())
}

fn refresh_pointers(dir: &Path, records: &[RunRecord]) -> Result<()> {
    let newest = records.iter().max_by_key(|record| record.started_ms);
    update_pointer(dir, LAST_FILE, newest.map(|record| record.run_id.as_str()))?;
    let newest_failed = records
        .iter()
        .filter(|record| record.state == "failed")
        .max_by_key(|record| record.started_ms);
    update_pointer(
        dir,
        LAST_FAILED_FILE,
        newest_failed.map(|record| record.run_id.as_str()),
    )
}

fn update_pointer(dir: &Path, name: &str, run_id: Option<&str>) -> Result<()> {
    match run_id {
        Some(run_id) => run_log::write_pointer(dir, name, run_id),
        None => {
            let path = dir.join(name);
            match fs::remove_file(&path) {
                Ok(()) => Ok(()),
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
                Err(source) => Err(Error::io("remove", &path, source)),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use std::path::{Path, PathBuf};

    use super::LogStore;

    /// A Btrfs-root host (longest mount `/`) keeps the store under the user's data directory.
    #[test]
    fn a_root_mount_resolves_under_the_data_home() {
        let root = LogStore::root_for(
            &[PathBuf::from("/")],
            Path::new("/srv/demo"),
            Path::new("/home/you/.local/share"),
            true,
        );
        assert_eq!(root, PathBuf::from("/home/you/.local/share/slipfork/store"));
    }

    /// A writable non-`/` Btrfs mount keeps `<mount>/.slipfork-store`.
    #[test]
    fn a_writable_nested_mount_keeps_the_mount_store() {
        let root = LogStore::root_for(
            &[PathBuf::from("/"), PathBuf::from("/mnt/cow")],
            Path::new("/mnt/cow/demo"),
            Path::new("/home/you/.local/share"),
            true,
        );
        assert_eq!(root, PathBuf::from("/mnt/cow/.slipfork-store"));
    }

    /// A read-only nested mount also falls back to the data directory.
    #[test]
    fn a_read_only_mount_falls_back_to_the_data_home() {
        let root = LogStore::root_for(
            &[PathBuf::from("/mnt/ro")],
            Path::new("/mnt/ro/demo"),
            Path::new("/var/lib/data"),
            false,
        );
        assert_eq!(root, PathBuf::from("/var/lib/data/slipfork/store"));
    }
}
