//! One world's `PGDATA` clone plus the pod that serves it on the world's unix socket.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use crate::engine::readiness;
use crate::error::{Error, Result};
use crate::layout::{StoreLayout, WorldName};
use crate::pod::{Bind, BindSource, PodEnv, PodSpec};
use crate::port::WorldEngine;
use crate::resources::{ImageFacts, ResourceKind, binary_in};

use super::{DEV_FLAGS, PGDATA, POSTGRES_CANDIDATES, SOCKET_DIR};

/// A `PGDATA` clone plus the snapshot latency that produced it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PostgresClone {
    /// `<store>/worlds/<world>.pg.<pod>`.
    pub path: PathBuf,
    /// Time inside `BTRFS_IOC_SNAP_CREATE_V2`; zero when an existing clone was reused.
    pub elapsed: Duration,
}

/// One world's `PGDATA` clone plus the pod that serves it on the world's unix socket.
#[derive(Debug, Clone)]
pub struct PostgresLauncher {
    layout: StoreLayout,
    world: WorldName,
}

impl PostgresLauncher {
    /// Readiness probes fire every `2ms`; the spec's promise, never a fixed sleep.
    pub const READINESS_INTERVAL: Duration = readiness::POLL_INTERVAL;
    /// A postgres pod must answer inside this window or the boot fails closed.
    pub const READINESS_BUDGET: Duration = readiness::READINESS_BUDGET;
    /// The protocol port the pod's unix socket is named after (`.s.PGSQL.5432`).
    pub const PORT: u16 = 5432;

    /// Binds the launcher to one store and one world.
    #[must_use]
    pub fn new(layout: StoreLayout, world: WorldName) -> Self {
        Self { layout, world }
    }

    /// `<store>/worlds/<world>.pg.<pod>`: the layout owns the flat-sibling format.
    #[must_use]
    pub fn clone_path(&self, pod: &str) -> PathBuf {
        self.layout
            .world_clone(&self.world, ResourceKind::Postgres, pod)
    }

    /// Host path of the pod's unix socket (`<clone>/.sock/.s.PGSQL.5432`).
    #[must_use]
    pub fn socket_path(&self, pod: &str) -> PathBuf {
        self.clone_path(pod).join(SOCKET_DIR).join(".s.PGSQL.5432")
    }

    /// Snapshots the sealed template once per world and unlinks inherited runtime files.
    ///
    /// `elapsed` is `Duration::ZERO` when an existing clone is reused, so a reboot never pays a
    /// snapshot twice and never reports a fabricated latency.
    pub fn clone(
        &self,
        engine: &dyn WorldEngine,
        template: &Path,
        pod: &str,
    ) -> Result<PostgresClone> {
        let path = self.clone_path(pod);
        let elapsed = if path.exists() {
            Duration::ZERO
        } else {
            let started = Instant::now();
            engine.clone_subvolume(template, &path)?;
            started.elapsed()
        };
        clear_stale_state(&path)?;
        let socket = path.join(SOCKET_DIR);
        fs::create_dir_all(&socket).map_err(|source| Error::io("create", &socket, source))?;
        Ok(PostgresClone { path, elapsed })
    }

    /// Renders the pod: `postgres -D /var/lib/postgresql/data` directly, never the entrypoint.
    ///
    /// The binary is resolved inside the rootfs and exec'd by absolute path, so the pod never
    /// depends on the image's `PATH`; a missing `postgres` fails closed before a bundle is drawn.
    pub fn spec(
        &self,
        pod: &str,
        rootfs: &Path,
        shm: &Path,
        facts: &ImageFacts,
    ) -> Result<PodSpec> {
        let binary = binary_in(rootfs, &POSTGRES_CANDIDATES).ok_or_else(|| Error::MissingTool {
            tool: "postgres",
            path: rootfs.join(POSTGRES_CANDIDATES[0]),
        })?;
        let mut argv = vec![binary, "-D".to_owned(), PGDATA.to_owned()];
        argv.extend(DEV_FLAGS.iter().map(|flag| (*flag).to_owned()));
        Ok(PodSpec {
            id: pod.to_owned(),
            rootfs: rootfs.to_path_buf(),
            workdir: "/".to_owned(),
            argv,
            inherited_env: facts.env.clone(),
            env: vec![PodEnv::new("POSTGRES_HOST_AUTH_METHOD", "trust")],
            binds: vec![
                Bind {
                    source: BindSource::Host(self.clone_path(pod)),
                    destination: PGDATA.to_owned(),
                    read_only: false,
                },
                Bind {
                    source: BindSource::Host(shm.to_path_buf()),
                    destination: "/dev/shm".to_owned(),
                    read_only: false,
                },
            ],
            files: Vec::new(),
        })
    }

    /// Polls the pod's `.sock/.s.PGSQL.5432` every [`Self::READINESS_INTERVAL`] until `probe`
    /// accepts, and reports the wait.
    ///
    /// The probe is injected because only the composition root can reach the world unix socket.
    pub fn await_ready(&self, pod: &str, probe: &mut dyn FnMut() -> bool) -> Result<Duration> {
        readiness::wait_until(
            probe,
            "postgres",
            &self.socket_path(pod).display().to_string(),
            Self::READINESS_INTERVAL,
            Self::READINESS_BUDGET,
        )
    }
}

/// Unlinks the runtime files a cloned `PGDATA` inherits from a running parent.
pub(super) fn clear_stale_state(data: &Path) -> Result<()> {
    for name in ["postmaster.pid", "postmaster.opts"] {
        let path = data.join(name);
        match fs::remove_file(&path) {
            Ok(()) => {}
            Err(source) if source.kind() == io::ErrorKind::NotFound => {}
            Err(source) => return Err(Error::io("remove", &path, source)),
        }
    }
    let stats = data.join("pg_stat");
    if stats.is_dir() {
        fs::remove_dir_all(&stats).map_err(|source| Error::io("remove", &stats, source))?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::ffi::OsStr;

    use super::*;
    use crate::resources::pg::PostgresTemplate;

    #[test]
    fn dev_flags_are_exactly_the_spec_set() {
        assert_eq!(
            DEV_FLAGS,
            [
                "-c",
                "fsync=off",
                "-c",
                "synchronous_commit=off",
                "-c",
                "full_page_writes=off",
                "-c",
                "wal_level=minimal",
                "-c",
                "max_wal_senders=0",
                "-c",
                "autovacuum=off",
                "-c",
                "shared_buffers=32MB",
                "-c",
                "dynamic_shared_memory_type=posix",
                "-c",
                "unix_socket_directories=/var/lib/postgresql/data/.sock",
                "-c",
                "listen_addresses=",
            ]
        );
        assert_eq!(PGDATA, "/var/lib/postgresql/data");
        assert_eq!(PostgresLauncher::PORT, 5432);
        assert_eq!(
            PostgresLauncher::READINESS_INTERVAL,
            Duration::from_millis(2)
        );
        assert_eq!(PostgresLauncher::READINESS_BUDGET, Duration::from_secs(2));
    }

    #[test]
    fn stale_locks_and_pg_stat_are_unlinked_but_the_cluster_survives() {
        let dir = tempfile::tempdir().expect("disk temp dir");
        fs::write(dir.path().join("postmaster.pid"), "4242\n").expect("plant a lock");
        fs::write(dir.path().join("postmaster.opts"), "-D data\n").expect("plant opts");
        fs::write(dir.path().join("PG_VERSION"), "18\n").expect("seed the cluster");
        fs::create_dir_all(dir.path().join("pg_stat")).expect("seed runtime statistics");
        clear_stale_state(dir.path()).expect("strip inherited runtime files");
        assert!(!dir.path().join("postmaster.pid").exists());
        assert!(!dir.path().join("postmaster.opts").exists());
        assert!(
            !dir.path().join("pg_stat").exists(),
            "runtime stats must go"
        );
        assert!(
            dir.path().join("PG_VERSION").is_file(),
            "cluster data must never be touched"
        );
    }

    #[test]
    fn clones_are_flat_siblings_of_the_world() {
        let layout = StoreLayout::at("/mnt/btrfs/.slipfork-store");
        let world = WorldName::parse("demo").expect("valid world");
        let launcher = PostgresLauncher::new(layout.clone(), world.clone());
        assert_eq!(
            launcher.clone_path("pg"),
            Path::new("/mnt/btrfs/.slipfork-store/worlds/demo.pg.pg")
        );
        assert!(
            !launcher
                .clone_path("pg")
                .starts_with("/mnt/btrfs/.slipfork-store/worlds/demo/")
        );
        let parsed = StoreLayout::parse_world_clone(OsStr::new("demo.pg.pg"))
            .expect("the layout owns the clone name");
        assert_eq!(parsed.0, world);
        assert_eq!(parsed.1, ResourceKind::Postgres);
        assert_eq!(parsed.2, "pg");
        assert!(StoreLayout::parse_world_clone(OsStr::new("demo.pgx.pg")).is_none());
        assert!(StoreLayout::parse_world_clone(OsStr::new("demo.pg.")).is_none());
        let template = PostgresTemplate::new(layout);
        assert_eq!(
            template.path(),
            Path::new("/mnt/btrfs/.slipfork-store/templates/postgres-18")
        );
        assert_eq!(
            template.stamp(),
            Path::new("/mnt/btrfs/.slipfork-store/templates/postgres-18/SLIPFORK_TEMPLATE_VERSION")
        );
    }
}
