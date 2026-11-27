//! World lifecycle: postgres checkpoint, store flush, liveness, stop, and rootfs sweeps.

use std::fs;
use std::sync::atomic::Ordering;

use slipfork_world::{Dirtiness, PostgresLauncher, WorldEngine};

use super::WorldRuntime;

impl WorldRuntime {
    /// Runs one `CHECKPOINT` inside every live postgres pod, over its readiness-proven socket.
    ///
    /// Dials `127.0.0.1:5432` through `<pods>/control.sock` (`SCM_RIGHTS`) and speaks the minimal
    /// PostgreSQL v3 wire protocol, so the checkpoint costs `< 5ms` with zero `crun`/`psql` spawn.
    /// The clone keeps `fsync=off`, so this is what makes the next `fork` snapshot durable data.
    pub(in crate::world) fn checkpoint_postgres(&self) -> Result<(), String> {
        PostgresLauncher::new(self.layout.clone(), self.world.clone())
            .checkpoint_live(&self.pods_dir)
            .map(|_| ())
            .map_err(|error| error.to_string())
    }

    /// Flushes the store filesystem after the checkpoint so the next fork snapshots durable data.
    pub(in crate::world) fn flush_store(&self) -> Result<(), String> {
        self.engine.flush_store().map_err(|error| error.to_string())
    }

    /// Records that at least one long-lived pod or native S3 endpoint entered the world.
    pub(super) fn mark_live_world(&self) {
        self.live_world.store(true, Ordering::Release);
    }

    /// Whether any resource or service entered the world; a cold build/hook failure with `false`
    /// tears the empty world down immediately instead of orphaning `slipfork-world-init`.
    pub(in crate::world) fn has_live_world(&self) -> bool {
        self.live_world.load(Ordering::Acquire)
    }

    /// Stops an empty world immediately and sweeps ephemeral pod rootfs clones.
    pub(in crate::world) fn stop_and_cleanup(&self) -> Result<(), String> {
        let stopped = self
            .supervisor
            .lock()
            .map_err(|_| "world supervisor lock poisoned".to_owned())?
            .stop()
            .map_err(|error| error.to_string());
        self.sweep_clones();
        stopped
    }

    /// Blocks until the world init exits, then sweeps ephemeral pod rootfs clones.
    pub(in crate::world) fn await_exit_and_cleanup(&self) -> Result<(), String> {
        let awaited = self
            .supervisor
            .lock()
            .map_err(|_| "world supervisor lock poisoned".to_owned())?
            .await_exit()
            .map_err(|error| error.to_string());
        self.sweep_clones();
        awaited
    }

    fn sweep_clones(&self) {
        let clones = self
            .clones
            .lock()
            .map(|mut guard| std::mem::take(&mut *guard))
            .unwrap_or_default();
        for clone in clones {
            let _ = self.engine.destroy(&clone, Dirtiness::Clean, true);
        }
        let _ = fs::remove_dir(self.layout.world_pods(&self.world));
        let _ = fs::remove_dir(self.layout.pods_root());
    }
}
