//! One owner for the store root and every world run-capture path and catalog.
//!
//! Every caller injects the mount table, the project directory, `$XDG_DATA_HOME`, and the selected
//! mount's writability; this module never reads the environment or probes the host. A store root
//! selected on the Btrfs filesystem root (`/`) or on a read-only mount resolves under
//! `<data_home>/slipfork/store` instead of producing `/.slipfork-store`, and `<store>/logs/<world>`
//! is derived through [`StoreLayout`].

use std::fs::{self, DirBuilder, OpenOptions};
use std::io::Write;
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt};
use std::path::{Path, PathBuf};

use crate::error::{Error, Result};
use crate::layout::{STORE_DIR, StoreLayout, WorldName};
use crate::pod_log;
use crate::run_log::{self, INDEX_FILE, LAST_FAILED_FILE, LAST_FILE, RunRecord};

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

    /// Every recorded run, newest-wins per `run_id`, oldest first, with skipped-line count.
    ///
    /// Lines written by an older schema or damaged by a partial write are counted and skipped, so
    /// one bad line can never brick `slipfork logs`.
    pub fn records(&self) -> Result<run_log::Catalog> {
        run_log::catalog(&self.run_dir())
    }

    /// Prunes all but the newest `newest` non-failed runs, returning how many were removed.
    ///
    /// A `failed` record and its `.log`/`.idx` are never deleted: a failed run is the one an
    /// operator needs to inspect after a teardown.
    pub fn retain(&self, newest: usize) -> Result<usize> {
        let dir = self.run_dir();
        let mut records = run_log::catalog(&dir)?.records;
        records.sort_by_key(|record| record.started_ms);
        let non_failed = records
            .iter()
            .filter(|record| record.state != "failed")
            .count();
        let prune_budget = non_failed.saturating_sub(newest);
        let mut dropped_so_far = 0;
        let mut kept = Vec::with_capacity(records.len());
        let mut pruned = 0;
        for record in records {
            let keep = record.state == "failed" || dropped_so_far >= prune_budget;
            if keep {
                kept.push(record);
            } else {
                dropped_so_far += 1;
                remove_capture(&dir, &record.run_id)?;
                pruned += 1;
            }
        }
        if pruned > 0 {
            rewrite_index(&dir, &kept)?;
            refresh_pointers(&dir, &kept)?;
        }
        Ok(pruned)
    }
}

/// Appends one `RunRecord` as a single `index.jsonl` line, creating the run directory mode `0700`.
pub(crate) fn append_record(dir: &Path, record: &RunRecord) -> Result<()> {
    DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(dir)
        .map_err(|source| Error::io("create", dir, source))?;
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

fn remove_capture(dir: &Path, run_id: &str) -> Result<()> {
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

fn rewrite_index(dir: &Path, records: &[RunRecord]) -> Result<()> {
    let path = dir.join(INDEX_FILE);
    let temporary = path.with_extension("tmp");
    let mut file = OpenOptions::new()
        .create(true)
        .truncate(true)
        .write(true)
        .mode(0o600)
        .open(&temporary)
        .map_err(|source| Error::io("create", &temporary, source))?;
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
    fs::rename(&temporary, &path).map_err(|source| Error::io("rename", &path, source))
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
