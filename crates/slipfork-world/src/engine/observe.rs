//! Observed disk facts feeding the pure convergence planner (`specs/03` §7.4).
//!
//! [`super::BtrfsEngine::observe`] reads only this store's directories and lease files; the
//! composition root proves supervisor liveness (`/proc`) and passes the pid set in, so this module
//! never probes the environment or the kernel.

use std::collections::{BTreeMap, BTreeSet};
use std::ffi::OsStr;
use std::fs;
use std::io;
use std::path::Path;

use crate::error::{Error, Result};
use crate::layout::{self, ForkName, StoreLayout, WorldName};
use crate::ledger::{
    ConvergeInput, ForkRow, LeaseState, LedgerRows, OrphanPods, OrphanWorld, UnregisteredFork,
    UntrackedKind, WorldFacts, WorldRow,
};
use crate::runtime_dir::RuntimePods;
use crate::snapshot::{LEASE_FILE, Lease};

impl super::BtrfsEngine {
    /// Observed facts for `ledger::plan_converge`; `alive` holds supervisor pids the composition
    /// root proved live, and `runtime_dir` is the tmpfs root holding `<runtime_dir>/slipfork/pods`.
    ///
    /// Only rows whose `store_root` is this engine's store are considered, so isolated daemons
    /// sharing a mount never settle each other's worlds. The per-user runtime pod root is shared
    /// by every store one daemon owns, so pod orphaning is family-scoped to this store's roots and
    /// never reports a world any row knows.
    ///
    /// # Errors
    ///
    /// A malformed row name or an unreadable `worlds/`, `.forks/`, or `pods/` directory fails
    /// closed.
    pub fn observe(
        &self,
        rows: &LedgerRows,
        alive: &BTreeSet<i64>,
        runtime_dir: &Path,
    ) -> Result<ConvergeInput> {
        let store_root = self.layout.store_root();
        let worlds: Vec<WorldRow> = rows
            .worlds
            .iter()
            .filter(|row| row.store_root == store_root)
            .cloned()
            .collect();
        let forks = rows.forks.clone();
        let mut facts = BTreeMap::new();
        for row in &worlds {
            facts.insert(row.id, self.world_facts(row, alive, runtime_dir)?);
        }
        let untracked = self.untracked_forks(&worlds, &forks)?;
        let orphan_worlds = self.orphan_worlds(&worlds, &forks)?;
        let orphan_pods = self.orphan_pods(rows, &worlds, runtime_dir)?;
        Ok(ConvergeInput {
            worlds,
            forks,
            facts,
            untracked,
            orphan_worlds,
            orphan_pods,
        })
    }

    /// What the store proves about one world row.
    fn world_facts(
        &self,
        row: &WorldRow,
        alive: &BTreeSet<i64>,
        runtime_dir: &Path,
    ) -> Result<WorldFacts> {
        let world = WorldName::parse(&row.world_name)?;
        let data = self.layout.world(&world);
        let world_data_exists = data.exists() || !self.world_clones(&world)?.is_empty();
        let lease_path = data.join(LEASE_FILE);
        let lease = match fs::metadata(&lease_path) {
            Ok(_) if Lease::read(&lease_path).is_ok() => LeaseState::Present,
            Ok(_) => LeaseState::Corrupt,
            Err(_) => LeaseState::Absent,
        };
        let mut pod_dirs = Vec::new();
        if let Some(pods) = &row.pods_dir
            && pods.is_dir()
        {
            pod_dirs.push(pods.clone());
        }
        let store_pods = self.layout.world_pods(&world);
        if store_pods.is_dir() && !pod_dirs.contains(&store_pods) {
            pod_dirs.push(store_pods);
        }
        // A row with a stale/NULL `pods_dir` still owns the canonical tmpfs root (§7.4 rows 2/5).
        let runtime_pods = RuntimePods::world_root(runtime_dir, &world);
        if runtime_pods.is_dir() && !pod_dirs.contains(&runtime_pods) {
            pod_dirs.push(runtime_pods);
        }
        Ok(WorldFacts {
            checkout_exists: row.checkout_path.is_dir(),
            world_data_exists,
            supervisor_alive: row.supervisor_pid.is_some_and(|pid| alive.contains(&pid)),
            lease,
            pod_dirs,
        })
    }

    /// Every `<root>/.forks/<name>` entry that has no `forks` row in this store.
    fn untracked_forks(
        &self,
        worlds: &[WorldRow],
        forks: &[ForkRow],
    ) -> Result<Vec<UnregisteredFork>> {
        let tracked: BTreeSet<i64> = forks.iter().map(|fork| fork.world_id).collect();
        let mut known: BTreeMap<i64, BTreeSet<&str>> = BTreeMap::new();
        for fork in forks {
            known
                .entry(fork.root_id)
                .or_default()
                .insert(fork.name.as_str());
        }
        let mut untracked = Vec::new();
        for root in worlds.iter().filter(|row| !tracked.contains(&row.id)) {
            if !root.checkout_path.is_dir() {
                continue;
            }
            let root_world = WorldName::parse(&root.world_name)?;
            let forks_dir = layout::project_forks_dir(&root.checkout_path);
            let entries = match fs::read_dir(&forks_dir) {
                Ok(entries) => entries,
                Err(source) if source.kind() == io::ErrorKind::NotFound => continue,
                Err(source) => return Err(Error::io("read", &forks_dir, source)),
            };
            for entry in entries {
                let entry = entry.map_err(|source| Error::io("read", &forks_dir, source))?;
                let path = entry.path();
                if !path.is_dir() {
                    continue;
                }
                let Some(name) = entry.file_name().to_str().map(str::to_owned) else {
                    continue;
                };
                if name.starts_with(".partial-") {
                    untracked.push(UnregisteredFork {
                        root_id: root.id,
                        name,
                        checkout_path: path,
                        kind: UntrackedKind::Partial,
                    });
                    continue;
                }
                let Ok(fork) = ForkName::parse(&name) else {
                    continue;
                };
                if known
                    .get(&root.id)
                    .is_some_and(|names| names.contains(name.as_str()))
                {
                    continue;
                }
                let empty = match fs::read_dir(&path) {
                    Ok(mut entries) => entries.next().is_none(),
                    Err(source) if source.kind() == io::ErrorKind::NotFound => continue,
                    Err(source) => return Err(Error::io("read", &path, source)),
                };
                let kind = if empty {
                    UntrackedKind::Stub
                } else {
                    UntrackedKind::Checkout {
                        has_forkfile: path.join("Forkfile").is_file(),
                        parent_hint: self.parent_hint(&root_world, &fork),
                    }
                };
                untracked.push(UnregisteredFork {
                    root_id: root.id,
                    name,
                    checkout_path: path,
                    kind,
                });
            }
        }
        untracked.sort_by(|left, right| {
            (left.root_id, left.name.as_str()).cmp(&(right.root_id, right.name.as_str()))
        });
        Ok(untracked)
    }

    /// The lease origin of `<store>/worlds/<fork>--<root>`, when it names a different world.
    fn parent_hint(&self, root_world: &WorldName, fork: &ForkName) -> Option<String> {
        let world = StoreLayout::fork_world_name(root_world, fork);
        let lease = Lease::read(&self.layout.world(&world).join(LEASE_FILE)).ok()?;
        (!lease.origin.is_empty() && lease.origin != world.as_str()).then_some(lease.origin)
    }

    /// Every `worlds/<fork>--<root>*` entry whose root row exists while its checkout and row
    /// both vanish. Root subvolumes are never reported.
    fn orphan_worlds(&self, worlds: &[WorldRow], forks: &[ForkRow]) -> Result<Vec<OrphanWorld>> {
        let tracked: BTreeSet<i64> = forks.iter().map(|fork| fork.world_id).collect();
        let mut roots: BTreeMap<&str, &Path> = BTreeMap::new();
        for row in worlds.iter().filter(|row| !tracked.contains(&row.id)) {
            roots.insert(row.world_name.as_str(), row.checkout_path.as_path());
        }
        let store_root = self.layout.store_root();
        let worlds_root = self.layout.worlds_root();
        let entries = match fs::read_dir(&worlds_root) {
            Ok(entries) => entries,
            Err(source) if source.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(source) => return Err(Error::io("read", &worlds_root, source)),
        };
        let mut found: BTreeSet<String> = BTreeSet::new();
        for entry in entries {
            let entry = entry.map_err(|source| Error::io("read", &worlds_root, source))?;
            let name = entry.file_name();
            let candidate = StoreLayout::parse_fork_world(&name).or_else(|| {
                StoreLayout::parse_world_clone(&name).and_then(|(world, _, _)| {
                    StoreLayout::parse_fork_world(OsStr::new(world.as_str()))
                })
            });
            let Some((fork, root_world)) = candidate else {
                continue;
            };
            let Some(checkout) = roots.get(root_world.as_str()) else {
                continue;
            };
            let id = StoreLayout::fork_world_name(&root_world, &fork);
            if worlds.iter().any(|row| row.world_name == id.as_str()) {
                continue;
            }
            if layout::project_fork_path(checkout, &fork).exists() {
                continue;
            }
            found.insert(id.as_str().to_owned());
        }
        Ok(found
            .into_iter()
            .map(|name| OrphanWorld {
                store_root: store_root.clone(),
                name,
            })
            .collect())
    }

    /// Ephemeral pod directories (`specs/03` §7.4 row 6) whose world has no row in any store.
    ///
    /// The shared tmpfs runtime root is family-scoped to this store's roots, while `<store>/pods`
    /// is scanned for leftover world directories; both settle exactly once.
    fn orphan_pods(
        &self,
        rows: &LedgerRows,
        worlds: &[WorldRow],
        runtime_dir: &Path,
    ) -> Result<Vec<OrphanPods>> {
        let known: BTreeSet<&str> = rows
            .worlds
            .iter()
            .map(|row| row.world_name.as_str())
            .collect();
        let fork_ids: BTreeSet<i64> = rows.forks.iter().map(|fork| fork.world_id).collect();
        let roots: BTreeSet<&str> = worlds
            .iter()
            .filter(|row| !fork_ids.contains(&row.id))
            .map(|row| row.world_name.as_str())
            .collect();
        let in_family = |name: &str| {
            roots.contains(name)
                || StoreLayout::parse_fork_world(OsStr::new(name))
                    .is_some_and(|(_, root)| roots.contains(root.as_str()))
        };
        let store_root = self.layout.store_root();
        let mut found: BTreeMap<String, OrphanPods> = BTreeMap::new();
        let runtime_root = RuntimePods::worlds_root(runtime_dir);
        let entries = match fs::read_dir(&runtime_root) {
            Ok(entries) => entries,
            Err(source) if source.kind() == io::ErrorKind::NotFound => {
                return self.store_orphan_pods(&mut found, &known, &store_root, &runtime_root);
            }
            Err(source) => return Err(Error::io("read", &runtime_root, source)),
        };
        for entry in entries {
            let entry = entry.map_err(|source| Error::io("read", &runtime_root, source))?;
            let Some(world) = parsed_world(&entry) else {
                continue;
            };
            if known.contains(world.as_str()) || !in_family(world.as_str()) {
                continue;
            }
            let mut pod_dirs = vec![entry.path()];
            let store_pods = self.layout.world_pods(&world);
            if store_pods.is_dir() {
                pod_dirs.push(store_pods);
            }
            found.insert(
                world.as_str().to_owned(),
                OrphanPods {
                    store_root: store_root.clone(),
                    world_name: world.as_str().to_owned(),
                    pod_dirs,
                },
            );
        }
        self.store_orphan_pods(&mut found, &known, &store_root, &runtime_root)
    }

    /// Adds `<store>/pods/<world>` leftovers absent from `found`, pairing the runtime dir when it
    /// exists; the merged result is deterministic and deduped.
    fn store_orphan_pods(
        &self,
        found: &mut BTreeMap<String, OrphanPods>,
        known: &BTreeSet<&str>,
        store_root: &Path,
        runtime_root: &Path,
    ) -> Result<Vec<OrphanPods>> {
        let pods_root = self.layout.pods_root();
        let entries = match fs::read_dir(&pods_root) {
            Ok(entries) => entries,
            Err(source) if source.kind() == io::ErrorKind::NotFound => {
                return Ok(orphan_pods_from(found));
            }
            Err(source) => return Err(Error::io("read", &pods_root, source)),
        };
        for entry in entries {
            let entry = entry.map_err(|source| Error::io("read", &pods_root, source))?;
            let Some(world) = parsed_world(&entry) else {
                continue;
            };
            if known.contains(world.as_str()) || found.contains_key(world.as_str()) {
                continue;
            }
            let mut pod_dirs = vec![entry.path()];
            let runtime_pods = runtime_root.join(world.as_str());
            if runtime_pods.is_dir() {
                pod_dirs.push(runtime_pods);
            }
            found.insert(
                world.as_str().to_owned(),
                OrphanPods {
                    store_root: store_root.to_path_buf(),
                    world_name: world.as_str().to_owned(),
                    pod_dirs,
                },
            );
        }
        Ok(orphan_pods_from(found))
    }
}

/// A directory entry whose name parses as a world id.
fn parsed_world(entry: &fs::DirEntry) -> Option<WorldName> {
    entry
        .file_type()
        .ok()
        .filter(fs::FileType::is_dir)
        .and_then(|_| {
            entry
                .file_name()
                .to_str()
                .and_then(|name| WorldName::parse(name).ok())
        })
}

/// Sorted, deduped `OrphanPods` in world-name order.
fn orphan_pods_from(found: &BTreeMap<String, OrphanPods>) -> Vec<OrphanPods> {
    found
        .values()
        .map(|pods| {
            let mut pod_dirs = pods.pod_dirs.clone();
            pod_dirs.sort();
            pod_dirs.dedup();
            OrphanPods {
                store_root: pods.store_root.clone(),
                world_name: pods.world_name.clone(),
                pod_dirs,
            }
        })
        .collect()
}
