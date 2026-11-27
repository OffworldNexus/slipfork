//! Container pods over `<pods>/control.sock` and their writable rootfs clones.

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::Ordering;

use slipfork_world::{Dirtiness, PodEnv, PodSpec, Stream, WorldEngine, control, write_bundle};

use super::WorldRuntime;
use super::helpers::merge_env;
use crate::world::heads::ImageHead;
use crate::world::supervise::BootSink;

impl WorldRuntime {
    /// Writes `<pods>/<id>/config.json` and launches the detached container concurrently over
    /// `<pods>/control.sock`, with zero supervisor lock held.
    pub(super) fn launch_pod(&self, spec: &PodSpec) -> Result<u32, String> {
        write_bundle(spec, &self.pod_host, &self.pods_dir.join(&spec.id))
            .map_err(|error| error.to_string())?;
        let pid = control::launch_pod(&self.pods_dir, spec, self.boot_timeout)
            .map_err(|error| error.to_string())?;
        self.mark_live_world();
        Ok(pid)
    }

    /// Runs one setup/hook/`initdb` pod over `<pods>/control.sock` with zero supervisor lock held.
    pub(super) fn run_pod_once(
        &self,
        spec: &PodSpec,
        on_line: &mut dyn FnMut(Stream, &str),
    ) -> slipfork_world::Result<i32> {
        write_bundle(spec, &self.pod_host, &self.pods_dir.join(&spec.id))?;
        control::run_once(&self.pods_dir, spec, self.boot_timeout, on_line)
    }

    /// Returns a writable pod rootfs subvolume: reuses a pre-snapshotted `<store>/pods/<world>/<slot>`
    /// from `fork` in 0ms when `reusable` is true, otherwise clones `sealed`.
    pub(super) fn clone_rootfs(
        &self,
        sealed: &Path,
        slot: &str,
        reusable: bool,
    ) -> Result<PathBuf, String> {
        let dir = self.layout.world_pods(&self.world);
        fs::create_dir_all(&dir).map_err(|error| format!("create `{}`: {error}", dir.display()))?;
        let dest = if reusable {
            let candidate = dir.join(slot);
            if slipfork_world::is_subvolume_path(&candidate).unwrap_or(false) {
                return Ok(candidate);
            }
            candidate
        } else {
            let seq = self.counter.fetch_add(1, Ordering::Relaxed) + 1;
            dir.join(format!("{slot}-{seq}"))
        };
        if slipfork_world::is_subvolume_path(&dest).unwrap_or(false) {
            self.engine
                .destroy(&dest, Dirtiness::Clean, true)
                .map_err(|error| error.to_string())?;
        } else if dest.is_dir() {
            fs::remove_dir(&dest)
                .map_err(|error| format!("remove `{}`: {error}", dest.display()))?;
        }
        self.engine
            .clone_subvolume(sealed, &dest)
            .map_err(|error| error.to_string())?;
        if let Ok(mut clones) = self.clones.lock() {
            clones.push(dest.clone());
        }
        Ok(dest)
    }

    pub(super) fn built_for(
        &self,
        image: Option<&String>,
        _log: &mut BootSink,
    ) -> Result<ImageHead, String> {
        let image = image.ok_or_else(|| "step has no enclosing image".to_owned())?;
        self.head_for(image)
    }

    /// The daemon-sealed head for `image`; a missing head fails closed with no fallback build.
    pub(super) fn head_for(&self, image: &str) -> Result<ImageHead, String> {
        self.heads.get(image).cloned().ok_or_else(|| {
            format!(
                "no sealed image head for `{image}`\nnext: run `slipfork up` to plan this world again"
            )
        })
    }

    /// Merges image-level and subject-level runtime `env`, plus the declared `PORT`.
    pub(super) fn pod_env_for(
        &self,
        subject: &str,
        image: Option<&str>,
        port: Option<u16>,
    ) -> Vec<PodEnv> {
        merge_env(&self.resolved_env, subject, image, port)
    }
}
