//! The store-global golden `initdb` cluster: built once, sealed `BTRFS_SUBVOL_RDONLY`.

use std::fs::{self, File, OpenOptions};
use std::io;
use std::os::fd::AsRawFd;
use std::path::{Path, PathBuf};
use std::thread;
use std::time::{Duration, Instant};

use crate::btrfs;
use crate::engine::readiness;
use crate::error::{Error, Result};
use crate::layout::StoreLayout;
use crate::pod::{Bind, BindSource, PodEnv, PodSpec};
use crate::port::{Dirtiness, WorldEngine};
use crate::resources::binary_in;
use crate::snapshot::NodeKey;

use super::launcher::clear_stale_state;
use super::{
    INITDB_CANDIDATES, PGDATA, TEMPLATE_NAME, TEMPLATE_STAMP, TEMPLATE_VERSION, TEMPLATES_DIR,
};

/// How long a losing `slipfork up` waits for the winning process to finish `initdb`.
const TEMPLATE_WAIT: Duration = Duration::from_secs(30);

/// Whether this run built the golden template or found it already sealed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TemplateState {
    /// This process ran the one `initdb` and sealed the cluster.
    Created,
    /// A sealed, version-stamped template already existed (or a racing process finished it).
    Cached,
}

/// Store-global `flock` guard for a first-time `initdb`; the kernel releases it on SIGKILL.
#[derive(Debug)]
struct InitLock {
    _held: File,
}

impl InitLock {
    /// `None` means another `slipfork up` owns the lock right now.
    #[allow(unsafe_code)]
    fn acquire(path: &Path) -> Result<Option<Self>> {
        let held = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(path)
            .map_err(|source| Error::io("lock", path, source))?;
        // SAFETY: `flock(2)` only consumes the live descriptor; it dereferences no memory.
        let rc = unsafe { libc::flock(held.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) };
        if rc == 0 {
            return Ok(Some(Self { _held: held }));
        }
        let source = io::Error::last_os_error();
        if matches!(source.raw_os_error(), Some(libc::EWOULDBLOCK)) {
            Ok(None)
        } else {
            Err(Error::io("lock", path, source))
        }
    }
}

/// The store-global golden `initdb` cluster, sealed once with `BTRFS_SUBVOL_RDONLY`.
#[derive(Debug, Clone)]
pub struct PostgresTemplate {
    layout: StoreLayout,
}

impl PostgresTemplate {
    /// Binds the template to one store; no path or global is read from the environment.
    #[must_use]
    pub fn new(layout: StoreLayout) -> Self {
        Self { layout }
    }

    /// `<store>/templates`.
    #[must_use]
    pub fn root(&self) -> PathBuf {
        self.layout.store_root().join(TEMPLATES_DIR)
    }

    /// `<store>/templates/postgres-18`: sealed, never written by a world.
    #[must_use]
    pub fn path(&self) -> PathBuf {
        self.root().join(TEMPLATE_NAME)
    }

    /// `<store>/templates/postgres-18/SLIPFORK_TEMPLATE_VERSION`, sealed with the cluster.
    #[must_use]
    pub fn stamp(&self) -> PathBuf {
        self.path().join(TEMPLATE_STAMP)
    }

    fn lock(&self) -> PathBuf {
        self.root().join(format!("{TEMPLATE_NAME}.lock"))
    }

    /// Builds the one `initdb` pod: curated `postgres` superuser, trust auth, no world roles.
    ///
    /// The curated dataset carries nothing world-specific, so every world (and every fork) can
    /// clone it; child databases are created later inside the world.
    pub fn initdb_spec(&self, pod: &str, rootfs: &Path, data: &Path) -> Result<PodSpec> {
        let seed = PgSeed::spec(rootfs)?;
        Ok(PodSpec {
            id: pod.to_owned(),
            rootfs: rootfs.to_path_buf(),
            workdir: "/".to_owned(),
            argv: seed.argv,
            inherited_env: Vec::new(),
            env: vec![PodEnv::new("POSTGRES_HOST_AUTH_METHOD", "trust")],
            binds: vec![Bind {
                source: BindSource::Host(data.to_path_buf()),
                destination: PGDATA.to_owned(),
                read_only: false,
            }],
            files: Vec::new(),
        })
    }

    /// Ensures the sealed template exists, running the real `initdb` pod at most once.
    ///
    /// The winner holds an OS `flock` on `<store>/templates/postgres-18.lock`, writes the version
    /// stamp **inside** staging, then promotes it with one atomic rename and seals it. A SIGKILL
    /// drops the kernel lock, so the next boot reaps staging and rebuilds instead of waiting 30s.
    /// `on_wait` reports the genuine first-`initdb` wait; a stale sealed-but-unstamped template is
    /// rebuilt under the held lock rather than served.
    pub fn ensure(
        &self,
        engine: &dyn WorldEngine,
        rootfs: &Path,
        run: &mut dyn FnMut(&PodSpec) -> Result<()>,
        on_wait: &mut dyn FnMut(Duration),
    ) -> Result<TemplateState> {
        fs::create_dir_all(self.root())
            .map_err(|source| Error::io("create", &self.root(), source))?;
        let started = Instant::now();
        loop {
            if self.is_ready(engine)? {
                // A crashed first `initdb` may have left staging behind even though the promoted
                // template is sealed; reap it under a short-lived lock so it never survives.
                if let Some(_lock) = InitLock::acquire(&self.lock())? {
                    sweep_staging(engine, &self.root())?;
                }
                return Ok(TemplateState::Cached);
            }
            let Some(_lock) = InitLock::acquire(&self.lock())? else {
                if started.elapsed() >= TEMPLATE_WAIT {
                    return Err(Error::TemplateWait { path: self.path() });
                }
                on_wait(started.elapsed());
                thread::sleep(readiness::POLL_INTERVAL);
                continue;
            };
            // A crashed run left a sealed or half-seeded template behind: unseal and rebuild it
            // under this lock, never `initdb` over it and never send the user to `rm -rf`.
            if self.path().exists() {
                force_remove(engine, &self.path())
                    .map_err(|_| Error::StaleTemplate { path: self.path() })?;
            }
            sweep_staging(engine, &self.root())?;
            let staging = self
                .root()
                .join(format!(".{TEMPLATE_NAME}.staging-{}", std::process::id()));
            return self.build(engine, rootfs, run, &staging);
        }
    }

    fn build(
        &self,
        engine: &dyn WorldEngine,
        rootfs: &Path,
        run: &mut dyn FnMut(&PodSpec) -> Result<()>,
        staging: &Path,
    ) -> Result<TemplateState> {
        let initdb = self.initdb_spec("postgres-initdb", rootfs, staging)?;
        engine.create_subvolume(staging)?;
        if let Err(error) = run(&initdb) {
            let _ = engine.destroy(staging, Dirtiness::Clean, true);
            return Err(error);
        }
        clear_stale_state(staging)?;
        fs::write(staging.join(TEMPLATE_STAMP), TEMPLATE_VERSION)
            .map_err(|source| Error::io("write", staging, source))?;
        fs::rename(staging, self.path()).map_err(|source| Error::io("promote", staging, source))?;
        engine.seal(&self.path())?;
        Ok(TemplateState::Created)
    }

    fn is_ready(&self, engine: &dyn WorldEngine) -> Result<bool> {
        let path = self.path();
        if !path.is_dir() || !btrfs::is_subvolume_path(&path)? {
            return Ok(false);
        }
        if !engine.is_sealed(&path)? {
            return Ok(false);
        }
        Ok(matches!(
            fs::read_to_string(self.stamp()),
            Ok(version) if version.trim() == TEMPLATE_VERSION
        ))
    }
}

/// The `initdb` argv a sealed `seed` node hashes, rootfs-absolute.
fn initdb_argv(initdb: String) -> Vec<String> {
    vec![
        initdb,
        "-D".to_owned(),
        PGDATA.to_owned(),
        "-U".to_owned(),
        "postgres".to_owned(),
        "--auth=trust".to_owned(),
        "--encoding=UTF8".to_owned(),
        "--no-sync".to_owned(),
    ]
}

/// The immutable value a `seed` node is built from: `initdb` argv + template version (§6.1).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SeedSpec {
    argv: Vec<String>,
    template_version: String,
}

impl SeedSpec {
    /// The exact `initdb` argv the seed subvolume is built with.
    #[must_use]
    pub fn argv(&self) -> &[String] {
        &self.argv
    }

    /// The template contract, bumped when the `initdb` invocation changes.
    #[must_use]
    pub fn template_version(&self) -> &str {
        &self.template_version
    }

    /// The content-addressed `seed` key over the `postgres` image it was built from.
    #[must_use]
    pub fn node_key(&self, arch: &str, image: &NodeKey) -> NodeKey {
        NodeKey::seed(arch, image, &self.argv, &self.template_version)
    }
}

/// The `postgres` seed node's value: argv discovery plus the template version, no pods, no disk.
#[derive(Debug, Clone, Copy, Default)]
pub struct PgSeed;

impl PgSeed {
    /// The template contract this build reproduces.
    pub const TEMPLATE_VERSION: &'static str = TEMPLATE_VERSION;

    /// Discovers `initdb` inside `rootfs` and freezes the argv a `seed` node hashes.
    pub fn spec(rootfs: &Path) -> Result<SeedSpec> {
        let initdb = binary_in(rootfs, &INITDB_CANDIDATES).ok_or_else(|| Error::MissingTool {
            tool: "initdb",
            path: rootfs.join(INITDB_CANDIDATES[0]),
        })?;
        Ok(SeedSpec {
            argv: initdb_argv(initdb),
            template_version: TEMPLATE_VERSION.to_owned(),
        })
    }
}

/// Unseals and destroys one subvolume, or removes a plain directory left by a crash.
fn force_remove(engine: &dyn WorldEngine, path: &Path) -> Result<()> {
    if !path.exists() {
        return Ok(());
    }
    if btrfs::is_subvolume_path(path).unwrap_or(false) {
        engine.destroy(path, Dirtiness::Clean, true)
    } else {
        fs::remove_dir_all(path).map_err(|source| Error::io("remove", path, source))
    }
}

/// Reaps `<templates>/.postgres-18.staging-*` before a new staging is created.
fn sweep_staging(engine: &dyn WorldEngine, root: &Path) -> Result<()> {
    let prefix = format!(".{TEMPLATE_NAME}.staging-");
    let entries = fs::read_dir(root).map_err(|source| Error::io("read", root, source))?;
    for entry in entries {
        let entry = entry.map_err(|source| Error::io("read", root, source))?;
        if entry
            .file_name()
            .to_str()
            .is_some_and(|name| name.starts_with(&prefix))
        {
            force_remove(engine, &entry.path())?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn init_lock_is_exclusive_until_dropped() {
        let dir = tempfile::tempdir().expect("disk temp dir");
        let path = dir.path().join("postgres-18.lock");
        let held = InitLock::acquire(&path).expect("lock").expect("first lock");
        assert!(
            InitLock::acquire(&path).expect("lock").is_none(),
            "a second creator must lose the race"
        );
        drop(held);
        assert!(
            InitLock::acquire(&path).expect("lock").is_some(),
            "dropping the guard must release the lock"
        );
    }
}
