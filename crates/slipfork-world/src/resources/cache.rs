//! World-local Redis AOF pods: live keys survive `fork` because `/data` lives in the world.

use std::fs;
use std::path::{Path, PathBuf};
use std::time::Duration;

use crate::engine::readiness;
use crate::error::{Error, Result};
use crate::layout::{StoreLayout, WorldName};
use crate::pod::{Bind, BindSource, PodSpec};

use super::{ImageFacts, binary_in};

/// The exact server argv: loopback only, AOF on, no RDB save, no fsync stalls.
const ARGV: [&str; 11] = [
    "redis-server",
    "--bind",
    "127.0.0.1",
    "--dir",
    "/data",
    "--save",
    "",
    "--appendonly",
    "yes",
    "--appendfsync",
    "no",
];
/// Rootfs-relative `redis-server` locations of the official image and distro builds.
const REDIS_CANDIDATES: [&str; 3] = [
    "usr/local/bin/redis-server",
    "usr/bin/redis-server",
    "bin/redis-server",
];
/// In-container AOF directory.
const DATA: &str = "/data";

/// Runs `redis-server` on `<world>/redis/<id>`, a plain directory inside the world subvolume.
#[derive(Debug, Clone)]
pub struct RedisLauncher {
    layout: StoreLayout,
    world: WorldName,
}

impl RedisLauncher {
    /// Readiness probes fire every `2ms`; the spec's promise, never a fixed sleep.
    pub const READINESS_INTERVAL: Duration = readiness::POLL_INTERVAL;
    /// A redis pod must answer inside this window or the boot fails closed.
    pub const READINESS_BUDGET: Duration = readiness::READINESS_BUDGET;
    /// The world-loopback port the launcher probes.
    pub const PORT: u16 = 6379;

    /// Binds the launcher to one store and one world.
    #[must_use]
    pub fn new(layout: StoreLayout, world: WorldName) -> Self {
        Self { layout, world }
    }

    /// `<store>/worlds/<world>/redis/<id>`: AOF files travel with the world on `fork`.
    #[must_use]
    pub fn data_dir(&self, id: &str) -> PathBuf {
        self.layout.world(&self.world).join("redis").join(id)
    }

    /// Creates the AOF directory when missing; a plain directory, never a nested subvolume.
    pub fn prepare(&self, id: &str) -> Result<PathBuf> {
        let dir = self.data_dir(id);
        fs::create_dir_all(&dir).map_err(|source| Error::io("create", &dir, source))?;
        Ok(dir)
    }

    /// Renders the pod: `redis-server --bind 127.0.0.1 --dir /data --save "" …` directly.
    ///
    /// The binary is resolved inside the rootfs and exec'd by absolute path, so the pod never
    /// depends on the image's `PATH`; a missing `redis-server` fails closed.
    pub fn spec(&self, id: &str, rootfs: &Path, facts: &ImageFacts) -> Result<PodSpec> {
        let binary = binary_in(rootfs, &REDIS_CANDIDATES).ok_or_else(|| Error::MissingTool {
            tool: "redis-server",
            path: rootfs.join(REDIS_CANDIDATES[0]),
        })?;
        let mut argv = vec![binary];
        argv.extend(ARGV.iter().skip(1).map(|arg| (*arg).to_owned()));
        Ok(PodSpec {
            id: id.to_owned(),
            rootfs: rootfs.to_path_buf(),
            workdir: "/".to_owned(),
            argv,
            inherited_env: facts.env.clone(),
            env: Vec::new(),
            binds: vec![Bind {
                source: BindSource::Host(self.data_dir(id)),
                destination: DATA.to_owned(),
                read_only: false,
            }],
            files: Vec::new(),
            identity: None,
        })
    }

    /// Polls `:6379` every [`Self::READINESS_INTERVAL`] until `probe` accepts, and reports the wait.
    ///
    /// The probe is injected because only the composition root can reach the world loopback.
    pub fn await_ready(&self, probe: &mut dyn FnMut() -> bool) -> Result<Duration> {
        readiness::wait_until(
            probe,
            "redis",
            ":6379",
            Self::READINESS_INTERVAL,
            Self::READINESS_BUDGET,
        )
    }
}
