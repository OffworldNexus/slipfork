//! Mark-and-sweep over one snapshots root (`specs/03-under-the-hood.md` §6.3).

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::PathBuf;

use super::key::NodeKey;
use super::lease::{LEASE_FILE, Lease};
use super::store::{LOCK_FILE, NodeRecord, Pin, SnapshotStore};
use super::{SnapshotError, SnapshotResult};

/// Unmarked nodes stay warm for 24h before gc sweeps them.
pub const GC_GRACE_MS: u64 = 24 * 60 * 60 * 1000;

/// What one sweep pruned and kept.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct GcReport {
    /// Swept node keys.
    pub pruned: Vec<NodeKey>,
    /// Nodes kept (marked or still inside the grace window).
    pub kept: usize,
}

/// Either a completed sweep or a fail-closed skip.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GcOutcome {
    /// The sweep ran.
    Swept(GcReport),
    /// Nothing was swept.
    Skipped {
        /// One-line reason (a running reconcile, or an unreadable lease).
        reason: String,
    },
}

impl GcOutcome {
    /// How many nodes were swept.
    #[must_use]
    pub fn pruned(&self) -> usize {
        match self {
            Self::Swept(report) => report.pruned.len(),
            Self::Skipped { .. } => 0,
        }
    }
}

/// The pure mark set: lease image/seed keys, heads, pins, plus every parent closure through records.
#[must_use]
pub fn gc_mark_set(
    leases: &[Lease],
    pins: &[Pin],
    records: &BTreeMap<NodeKey, NodeRecord>,
) -> BTreeSet<NodeKey> {
    let mut queue: Vec<NodeKey> = Vec::new();
    for lease in leases {
        queue.extend(lease.gc_mark());
        queue.push(lease.head);
    }
    for pin in pins {
        queue.extend(pin.keys().iter().copied());
        queue.push(pin.head());
    }
    let mut marked = BTreeSet::new();
    while let Some(key) = queue.pop() {
        if !marked.insert(key) {
            continue;
        }
        if let Some(record) = records.get(&key) {
            queue.extend(record.parents.iter().copied());
        }
    }
    marked
}

impl SnapshotStore<'_> {
    /// Sweeps unmarked nodes older than `grace_ms`; never touches `worlds/`, `pods/`, or `logs/`.
    pub fn gc(&self, now_ms: u64, grace_ms: u64, pins: &[Pin]) -> SnapshotResult<GcOutcome> {
        let Some(_lock) = self.try_lock_exclusive()? else {
            return Ok(GcOutcome::Skipped {
                reason: "a reconcile holds the snapshots lock".to_owned(),
            });
        };
        let leases = match self.read_leases() {
            Ok(leases) => leases,
            Err(reason) => return Ok(GcOutcome::Skipped { reason }),
        };
        let entries = match fs::read_dir(&self.root) {
            Ok(entries) => entries,
            Err(source) if source.kind() == std::io::ErrorKind::NotFound => {
                return Ok(GcOutcome::Swept(GcReport::default()));
            }
            Err(source) => return Err(SnapshotError::io("read", &self.root, source)),
        };
        let mut dirs: BTreeMap<NodeKey, PathBuf> = BTreeMap::new();
        let mut record_files: BTreeMap<NodeKey, PathBuf> = BTreeMap::new();
        let mut slots = Vec::new();
        for entry in entries {
            let entry = entry.map_err(|source| SnapshotError::io("read", &self.root, source))?;
            let path = entry.path();
            let Some(name) = entry.file_name().to_str().map(str::to_owned) else {
                continue;
            };
            if name == LOCK_FILE {
                continue;
            }
            if name.starts_with(".tmp-") {
                slots.push(path);
                continue;
            }
            if let Some(stem) = name.strip_suffix(".json") {
                if let Ok(key) = stem.parse::<NodeKey>() {
                    record_files.insert(key, path);
                }
                continue;
            }
            if let Ok(key) = name.parse::<NodeKey>() {
                dirs.insert(key, path);
            }
        }
        let mut records = BTreeMap::new();
        for (key, path) in &record_files {
            if let Ok(bytes) = fs::read(path)
                && let Ok(record) = serde_json::from_slice::<NodeRecord>(&bytes)
            {
                records.insert(*key, record);
            }
        }
        let marked = gc_mark_set(&leases, pins, &records);
        let keys: BTreeSet<NodeKey> = dirs.keys().chain(record_files.keys()).copied().collect();
        let mut pruned = Vec::new();
        let mut kept = 0usize;
        for key in keys {
            if marked.contains(&key) {
                kept += 1;
                continue;
            }
            let fresh = records
                .get(&key)
                .is_some_and(|record| now_ms.saturating_sub(record.last_used_ms) <= grace_ms);
            if fresh {
                kept += 1;
                continue;
            }
            if let Some(path) = dirs.get(&key) {
                self.destroy_path(path)?;
            }
            if let Some(path) = record_files.get(&key) {
                fs::remove_file(path)
                    .map_err(|source| SnapshotError::io("remove", path, source))?;
            }
            pruned.push(key);
        }
        for slot in &slots {
            self.destroy_path(slot)?;
        }
        Ok(GcOutcome::Swept(GcReport { pruned, kept }))
    }

    fn read_leases(&self) -> Result<Vec<Lease>, String> {
        let worlds = self.layout.worlds_root();
        if !worlds.is_dir() {
            return Ok(Vec::new());
        }
        let entries = fs::read_dir(&worlds)
            .map_err(|source| format!("could not read `{}`: {source}", worlds.display()))?;
        let mut leases = Vec::new();
        for entry in entries {
            let entry = entry
                .map_err(|source| format!("could not read `{}`: {source}", worlds.display()))?;
            if !entry.file_type().map(|kind| kind.is_dir()).unwrap_or(false) {
                continue;
            }
            let path = entry.path().join(LEASE_FILE);
            if !path.exists() {
                continue;
            }
            let lease = Lease::read(&path).map_err(|error| error.to_string())?;
            if !lease.is_consistent() {
                return Err(format!(
                    "lease `{}` head does not match its entries",
                    path.display()
                ));
            }
            leases.push(lease);
        }
        Ok(leases)
    }
}

#[cfg(test)]
mod tests {
    use super::super::{NodeKind, Parallelism};
    use super::*;

    fn key(byte: u8) -> NodeKey {
        NodeKey::from_digest([byte; 32])
    }

    fn record(parents: Vec<NodeKey>) -> NodeRecord {
        NodeRecord {
            v: 1,
            kind: NodeKind::Step,
            parents,
            canon: "RUN x".to_owned(),
            line: 1,
            created_ms: 0,
            last_used_ms: 0,
            facts: serde_json::Value::Null,
        }
    }

    #[test]
    fn mark_set_follows_leases_pins_and_records() {
        let image = key(9);
        let seed = key(8);
        let service = key(7);
        let parent = key(6);
        let step = key(5);
        let unmarked = key(4);
        let lease = Lease {
            v: 1,
            world: "demo".to_owned(),
            origin: "demo".to_owned(),
            world_epoch: "k".to_owned(),
            spec: "spec".to_owned(),
            head: key(3),
            seeds: vec![seed],
            services: BTreeMap::from([(
                "api".to_owned(),
                super::super::LeaseEntry {
                    key: service,
                    image,
                },
            )]),
            effects: BTreeMap::new(),
        };
        let mut records = BTreeMap::new();
        records.insert(step, record(vec![parent]));
        records.insert(parent, record(vec![]));
        let marked = gc_mark_set(
            &[lease],
            &[Pin::new(step, BTreeSet::from([step]))],
            &records,
        );
        for expected in [image, seed, step, parent] {
            assert!(marked.contains(&expected), "{expected} must be marked");
        }
        assert!(
            !marked.contains(&service),
            "gc marks entry images, not the service key itself"
        );
        assert!(!marked.contains(&unmarked));
        assert_eq!(Parallelism::new(0).get(), 1);
    }
}
