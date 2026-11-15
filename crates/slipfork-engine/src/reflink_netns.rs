//! Rootless Linux `FICLONE` + unprivileged namespace (`CLONE_NEWUSER | CLONE_NEWNS | CLONE_NEWNET |
//! CLONE_NEWPID`) implementation of [`WorldEngine`] (`R10`, `R11`, `ADR-0002`).
//!
//! World state is cloned with real `ioctl(FICLONE)` reflinks, host directories are bind-mounted
//! inside the world's mount namespace (read-write repo, read-only artifact volumes), and every
//! process keeps the exact host `$UID:$GID`. No `unsafe` code lives here: the only syscall module
//! is [`crate::linux_cow`].

#![forbid(unsafe_code)]

use std::collections::BTreeMap;
use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, mpsc};
use std::thread;

use slipfork_world::WorldTarget;

use crate::child_env;
use crate::linux_cow::reflink_tree;
use crate::{
    EngineError, ExecEnv, ExecOutput, MountMode, SnapshotId, SnapshotMode, StreamChunk,
    StreamSource, WorldEngine, WorldHandle, WorldMount, WorldSpec, WorldStatus, target_slug,
};

/// Mounts every [`WorldSpec::mounts`] entry inside the fresh mount namespace, then execs the
/// command. Mount triples (`mode host guest`) are positional arguments so no shell interpolation
/// of paths ever happens.
const WORLD_INIT_SCRIPT: &str = r#"set -e
ip link set lo up
while [ "$1" != "--" ]; do
  mode="$1"; host="$2"; guest="$3"; shift 3
  mount --bind "$host" "$guest"
  if [ "$mode" = "ro" ]; then
    mount -o remount,bind,ro "$guest"
  fi
done
shift
exec "$@"
"#;

/// Unprivileged Linux CoW world engine backed by `ioctl(FICLONE)` + isolated mount/net/pid
/// namespaces.
#[derive(Debug, Clone)]
pub struct ReflinkNetnsEngine {
    state_root: PathBuf,
    host_uid: u32,
    host_gid: u32,
    shared: Arc<Shared>,
}

#[derive(Debug)]
struct Shared {
    worlds: Mutex<BTreeMap<u64, WorldRecord>>,
    next_seq: AtomicU64,
}

impl Default for Shared {
    fn default() -> Self {
        Self {
            worlds: Mutex::new(BTreeMap::new()),
            next_seq: AtomicU64::new(0),
        }
    }
}

#[derive(Debug, Clone)]
struct WorldRecord {
    state_dir: PathBuf,
    base_env: ExecEnv,
    mounts: Vec<WorldMount>,
    status: WorldStatus,
}

impl ReflinkNetnsEngine {
    /// Construct a `ReflinkNetnsEngine` storing snapshots and cloned state under `state_root`
    /// (which MUST reside on a reflink-capable filesystem such as XFS `reflink=1` or Btrfs).
    pub fn new(state_root: impl AsRef<Path>, host_uid: u32, host_gid: u32) -> Self {
        Self {
            state_root: state_root.as_ref().to_path_buf(),
            host_uid,
            host_gid,
            shared: Arc::new(Shared::default()),
        }
    }

    fn snapshot_dir(&self, id: &SnapshotId) -> Result<PathBuf, EngineError> {
        Ok(self.state_root.join("snapshots").join(id.validate()?))
    }

    fn world_dir(&self, target: &WorldTarget, seq: u64) -> PathBuf {
        self.state_root
            .join("worlds")
            .join(format!("{}-{seq}", target_slug(target)))
    }

    fn lock(&self) -> Result<MutexGuard<'_, BTreeMap<u64, WorldRecord>>, EngineError> {
        self.shared.worlds.lock().map_err(|_| {
            EngineError::Execution("reflink-netns world registry lock is poisoned".to_owned())
        })
    }

    fn owned<'a>(
        &self,
        worlds: &'a mut BTreeMap<u64, WorldRecord>,
        world: &WorldHandle,
    ) -> Result<&'a mut WorldRecord, EngineError> {
        if !world.belongs_to(self.name()) {
            return Err(EngineError::WorldGone {
                world: format!("{world:?}"),
            });
        }
        worlds
            .get_mut(&world.seq())
            .ok_or_else(|| EngineError::WorldGone {
                world: format!("{world:?}"),
            })
    }

    /// Roll a failed seal back to `Live` so a transient filesystem error does not brick the world.
    fn reopen_after_failed_freeze(&self, world: &WorldHandle) {
        if let Ok(mut worlds) = self.lock()
            && let Ok(record) = self.owned(&mut worlds, world)
            && record.status == WorldStatus::Freezing
        {
            record.status = WorldStatus::Live;
        }
    }
}

impl WorldEngine for ReflinkNetnsEngine {
    fn name(&self) -> &'static str {
        "reflink-netns"
    }

    fn create_snapshot(
        &self,
        source_dir: &Path,
        id: &SnapshotId,
        mode: SnapshotMode,
    ) -> Result<(), EngineError> {
        if mode == SnapshotMode::StatefulRam {
            return Err(EngineError::StatefulRamUnsupported {
                engine: self.name(),
            });
        }
        let dst = self.snapshot_dir(id)?;
        if dst.exists() {
            fs::remove_dir_all(&dst).map_err(|e| EngineError::Io {
                path: dst.display().to_string(),
                message: e.to_string(),
            })?;
        }
        reflink_tree(source_dir, &dst)?;
        Ok(())
    }

    fn freeze_world(
        &self,
        world: &WorldHandle,
        id: &SnapshotId,
        mode: SnapshotMode,
    ) -> Result<(), EngineError> {
        if mode == SnapshotMode::StatefulRam {
            return Err(EngineError::StatefulRamUnsupported {
                engine: self.name(),
            });
        }
        let dst = self.snapshot_dir(id)?;
        let source = {
            let mut worlds = self.lock()?;
            let record = self.owned(&mut worlds, world)?;
            match record.status {
                WorldStatus::Live => {
                    record.status = WorldStatus::Freezing;
                    record.state_dir.clone()
                }
                WorldStatus::Freezing | WorldStatus::Frozen | WorldStatus::Destroyed => {
                    return Err(EngineError::WorldNotFreezable {
                        world: format!("{world:?}"),
                    });
                }
            }
        };

        let sealed = (|| {
            if dst.exists() {
                fs::remove_dir_all(&dst).map_err(|err| EngineError::Io {
                    path: dst.display().to_string(),
                    message: err.to_string(),
                })?;
            }
            reflink_tree(&source, &dst).map(|_| ())
        })();
        if let Err(err) = sealed {
            self.reopen_after_failed_freeze(world);
            return Err(err);
        }

        let mut worlds = self.lock()?;
        let record = self.owned(&mut worlds, world)?;
        if record.status == WorldStatus::Freezing {
            record.status = WorldStatus::Frozen;
            Ok(())
        } else {
            // The world was destroyed while the reflink was running: drop the half-claimed
            // snapshot and fail closed instead of resurrecting a destroyed world.
            let _ = fs::remove_dir_all(&dst);
            Err(EngineError::WorldNotFreezable {
                world: format!("{world:?}"),
            })
        }
    }

    fn clone_world(&self, spec: &WorldSpec) -> Result<WorldHandle, EngineError> {
        spec.validate()?;
        let src = self.snapshot_dir(&spec.snapshot)?;
        if !src.is_dir() {
            return Err(EngineError::SnapshotNotFound(spec.snapshot.0.clone()));
        }
        let home = spec
            .base_env
            .home()
            .ok_or_else(|| EngineError::InvalidWorldSpec {
                reason: "base_env must carry a world-scoped HOME".to_owned(),
            })?
            .to_path_buf();
        fs::create_dir_all(&home).map_err(|e| EngineError::Io {
            path: home.display().to_string(),
            message: e.to_string(),
        })?;

        let seq = self.shared.next_seq.fetch_add(1, Ordering::Relaxed) + 1;
        let world = WorldHandle::mint(self.name(), seq);
        let world_root = self.world_dir(&spec.target, seq);
        let state_dir = world_root.join("state");
        if let Err(err) = reflink_tree(&src, &state_dir) {
            let _ = fs::remove_dir_all(&world_root);
            return Err(err);
        }

        self.lock()?.insert(
            seq,
            WorldRecord {
                state_dir,
                base_env: spec.base_env.clone(),
                mounts: spec.mounts.clone(),
                status: WorldStatus::Live,
            },
        );
        Ok(world)
    }

    fn exec_in_world(
        &self,
        world: &WorldHandle,
        argv: &[String],
        cwd: &Path,
        overlay_env: &ExecEnv,
        on_output: &mut dyn FnMut(StreamChunk),
    ) -> Result<ExecOutput, EngineError> {
        if argv.is_empty() {
            return Err(EngineError::Execution(
                "exec argv must not be empty".to_owned(),
            ));
        }
        let (state_dir, mounts, env) = {
            let mut worlds = self.lock()?;
            let record = self.owned(&mut worlds, world)?;
            if record.status == WorldStatus::Destroyed {
                return Err(EngineError::WorldGone {
                    world: format!("{world:?}"),
                });
            }
            (
                record.state_dir.clone(),
                record.mounts.clone(),
                child_env::materialize(&record.base_env, overlay_env, |var| {
                    std::env::var(var.name()).ok()
                }),
            )
        };

        // Identity UID/GID mapping (`$UID -> $UID`) + `--keep-caps` preserves exact `$UID:$GID`
        // ownership on every file written to the shared workspace AND grants `CAP_SYS_ADMIN` /
        // `CAP_NET_ADMIN` inside the user namespace so mount namespaces stay private and
        // `ip link set lo up` brings up the world's own loopback.
        let mut cmd = Command::new("unshare");
        cmd.arg(format!("--map-user={}", self.host_uid))
            .arg(format!("--map-group={}", self.host_gid))
            .arg("--keep-caps")
            .arg("--mount")
            .arg("--net")
            .arg("--pid")
            .arg("--fork")
            .arg("sh")
            .arg("-c")
            .arg(WORLD_INIT_SCRIPT)
            .arg("slipfork-world-init");
        for mount in &mounts {
            cmd.arg(match mount.mode {
                MountMode::ReadOnly => "ro",
                MountMode::ReadWrite => "rw",
            })
            .arg(&mount.host_path)
            .arg(&mount.guest_path);
        }
        cmd.arg("--")
            .args(argv)
            .current_dir(cwd)
            .env_clear()
            .envs(&env)
            .env("SLIPFORK_WORLD_STATE", &state_dir)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());

        let mut child = cmd
            .spawn()
            .map_err(|e| EngineError::Execution(e.to_string()))?;
        let (sender, receiver) = mpsc::channel::<StreamChunk>();
        let mut readers = Vec::with_capacity(2);
        if let Some(stdout) = child.stdout.take() {
            readers.push(spawn_reader(stdout, StreamSource::Stdout, sender.clone()));
        }
        if let Some(stderr) = child.stderr.take() {
            readers.push(spawn_reader(stderr, StreamSource::Stderr, sender.clone()));
        }
        drop(sender);

        let mut stdout = Vec::new();
        let mut stderr = Vec::new();
        for chunk in receiver {
            match chunk.source {
                StreamSource::Stdout => stdout.extend_from_slice(&chunk.bytes),
                StreamSource::Stderr => stderr.extend_from_slice(&chunk.bytes),
            }
            on_output(chunk);
        }
        for reader in readers {
            let _ = reader.join();
        }
        let status = child
            .wait()
            .map_err(|e| EngineError::Execution(e.to_string()))?;
        Ok(ExecOutput {
            exit_code: status.code().unwrap_or(1),
            stdout: String::from_utf8_lossy(&stdout).into_owned(),
            stderr: String::from_utf8_lossy(&stderr).into_owned(),
        })
    }

    fn destroy_world(&self, world: WorldHandle) -> Result<(), EngineError> {
        let mut worlds = self.lock()?;
        let record = self.owned(&mut worlds, &world)?;
        if record.status == WorldStatus::Destroyed {
            return Err(EngineError::WorldGone {
                world: format!("{world:?}"),
            });
        }
        let root = record.state_dir.parent().map(Path::to_path_buf);
        record.status = WorldStatus::Destroyed;
        drop(worlds);
        if let Some(root) = root.filter(|root| root.exists()) {
            fs::remove_dir_all(&root).map_err(|e| EngineError::Io {
                path: root.display().to_string(),
                message: e.to_string(),
            })?;
        }
        Ok(())
    }
}

fn spawn_reader<R: Read + Send + 'static>(
    mut source: R,
    kind: StreamSource,
    sender: mpsc::Sender<StreamChunk>,
) -> thread::JoinHandle<()> {
    thread::spawn(move || {
        let mut buffer = [0_u8; 4096];
        while let Ok(read) = source.read(&mut buffer) {
            if read == 0 {
                break;
            }
            if sender
                .send(StreamChunk {
                    source: kind,
                    bytes: buffer[..read].to_vec(),
                })
                .is_err()
            {
                break;
            }
        }
    })
}
