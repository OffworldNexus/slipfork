//! Deciding and spawning one `up`: Idle by leased head, heal outside the lock, or reconcile.
//!
//! The head walks the build context, so it is computed before the daemon lock is taken; the pod
//! list that distinguishes Idle from heal is read only after the lock is released.

mod heal;
mod plan;

use std::fs::{self, OpenOptions};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use serde_json::Value;
use slipfork_world::{RuntimePods, WorldName};

use super::core::{Core, World};
use crate::daemon::Failure;
use crate::daemon::boot::BootSession;
use crate::daemon::lease::{self, Head};
use crate::daemon::rpc::{key_of, pid_alive, world_answers};

pub(crate) use heal::settle;
pub(crate) use plan::{Reconcile, reconcile};

/// What one `up` must do, decided under the daemon lock and executed outside it.
pub(crate) enum UpAction {
    /// `lease.head == desired head` and every service is bundled: `settle` prints the one
    /// `up to date` row when every pod is healthy, and restarts only the pods that are down.
    Idle(Box<Heal>),
    /// The leased head differs (or a service bundle is missing): stop the old world, boot anew.
    Reconcile,
    /// Nothing live; boot normally.
    Boot,
}

/// A head-matched world detached from [`Core`] so its health check never holds the daemon lock.
pub(crate) struct Heal {
    world: String,
    project: PathBuf,
    pods_dir: PathBuf,
    supervisor_pid: u32,
    started_at_ms: u64,
    fork: Option<String>,
}

/// The `project` parameter every lifecycle request carries.
pub(crate) fn project_path(params: &Value) -> Result<PathBuf, Failure> {
    params
        .get("project")
        .and_then(Value::as_str)
        .map(PathBuf::from)
        .ok_or_else(|| Failure::new("bad-params", "`project` is required"))
}

/// Whether an `up` may stay Idle: never with `--no-cache`, and only on a matching head whose
/// declared services already wrote their bundles.
fn is_idle(no_cache: bool, head_matches: bool, bundled: bool) -> bool {
    !no_cache && head_matches && bundled
}

impl Core {
    /// Decides one idempotent `up`: a matching lease stays Idle/heals; an edited head reconciles.
    pub(crate) fn up_plan(
        &mut self,
        params: &Value,
        desired: Option<&Head>,
    ) -> Result<UpAction, Failure> {
        let no_cache = params
            .get("no_cache")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        let project = project_path(params)?;
        let key = key_of(&project);
        let Some(World::Live { entry, .. }) = self.worlds.get(&key) else {
            return Ok(UpAction::Boot);
        };
        let entry = entry.clone();
        if !pid_alive(entry.supervisor_pid) || !world_answers(&entry.pods_dir) {
            return Ok(UpAction::Boot);
        }
        if is_idle(
            no_cache,
            self.head_matches(&entry.project, &entry.world, desired),
            plan::services_bundled(&entry.project, &entry.pods_dir),
        ) {
            return Ok(UpAction::Idle(Box::new(Heal {
                world: entry.world,
                project: entry.project,
                pods_dir: entry.pods_dir,
                supervisor_pid: entry.supervisor_pid,
                started_at_ms: entry.started_at_ms,
                fork: entry.fork,
            })));
        }
        // Do not detach yet: the daemon must pre-plan every image first, so a failed build leaves
        // the old world serving. The caller detaches after the heads are sealed.
        Ok(UpAction::Reconcile)
    }

    /// Whether the live world's leased head still equals the head planned for this checkout.
    pub(crate) fn head_matches(&self, project: &Path, world: &str, desired: Option<&Head>) -> bool {
        let Some(desired) = desired else {
            return false;
        };
        let Ok(world) = WorldName::parse(world) else {
            return false;
        };
        lease::read(&self.host.store_root_for(project), &world)
            .is_some_and(|lease| lease.head == desired.head)
    }

    /// Detaches the live world so `reconcile` and the reboot run outside the daemon lock.
    pub(crate) fn detach_live(&mut self, key: &str) -> Option<Reconcile> {
        let Some(World::Live { entry, child }) = self.worlds.remove(key) else {
            return None;
        };
        self.remove_lease(&entry.project, &entry.world);
        self.ingress = None;
        let _ = self.persist();
        Some(Reconcile {
            project: entry.project,
            pods_dir: entry.pods_dir,
            supervisor_pid: entry.supervisor_pid,
            child,
        })
    }
}

impl Core {
    /// Spawns `__supervise` and leaves a `Booting` placeholder; the returned session owns the boot.
    ///
    /// The caller holds one `Core` guard for the reservation, so a fork snapshot and its boot can
    /// never observe two different world maps.
    pub(crate) fn begin_up(
        &mut self,
        params: &Value,
        planned: lease::Planned,
    ) -> Result<BootSession, Failure> {
        let project = project_path(params)?;
        let key = key_of(&project);
        match self.worlds.get(&key) {
            Some(World::Booting { world, .. }) => {
                return Err(Failure::new(
                    "world-booting",
                    format!("world `{world}` is still starting"),
                ));
            }
            Some(World::Live { entry, .. })
                if pid_alive(entry.supervisor_pid) && world_answers(&entry.pods_dir) =>
            {
                return Err(Failure::new(
                    "world-running",
                    format!("world `{}` is already running", entry.world),
                ));
            }
            _ => {}
        }
        self.worlds.remove(&key);
        self.spawn(project, key, params, planned)
    }

    fn spawn(
        &mut self,
        project: PathBuf,
        key: String,
        params: &Value,
        planned: lease::Planned,
    ) -> Result<BootSession, Failure> {
        let requested = params.get("world").and_then(Value::as_str);
        let world = crate::world::world_name(&project, requested)
            .map_err(|message| Failure::new("invalid-world", message))?;
        let pods_dir = RuntimePods::world_root(&self.host.runtime_dir, &world);
        let log_dir = project.join(".slipfork");
        fs::create_dir_all(&log_dir).map_err(|error| {
            Failure::new(
                "log-dir",
                format!("create `{}`: {error}", log_dir.display()),
            )
        })?;
        let log_path = log_dir.join("supervisor.log");
        let log = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&log_path)
            .map_err(|error| {
                Failure::new(
                    "log-open",
                    format!("open `{}`: {error}", log_path.display()),
                )
            })?;
        let mut command = Command::new(&self.host.exe);
        command
            .arg("__supervise")
            .arg("--project")
            .arg(&project)
            .arg("--name")
            .arg(world.as_str())
            .arg("--heads")
            .arg(crate::world::WorldHeads::path(
                &self.host.runtime_dir,
                &world,
            ));
        if params.get("no_cache").and_then(Value::as_bool) == Some(true) {
            command.arg("--no-cache");
        }
        let child = command
            .current_dir(&project)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::from(log))
            .spawn()
            .map_err(|error| {
                Failure::new(
                    "supervisor-spawn",
                    format!("cannot start `__supervise`: {error}"),
                )
            })?;
        self.worlds.insert(
            key.clone(),
            World::Booting {
                project: project.clone(),
                world: world.as_str().to_owned(),
                pods_dir: pods_dir.clone(),
                supervisor_pid: child.id(),
            },
        );
        let store_root = self.host.store_root_for(&project);
        let mut session = BootSession::new(
            key,
            log_path,
            child,
            pods_dir,
            project.clone(),
            store_root,
            planned,
        );
        if let Some(fork) = layout_fork(&project) {
            session.reserve_fork(fork);
        }
        Ok(session)
    }
}

/// The fork name when `project` is the canonical `<parent>/.forks/<fork>` checkout.
fn layout_fork(project: &Path) -> Option<String> {
    let forks = project.parent()?;
    if !forks
        .file_name()
        .is_some_and(|name| name == slipfork_world::FORKS_DIR)
    {
        return None;
    }
    let fork = project.file_name()?.to_str()?;
    slipfork_world::ForkName::parse(fork)
        .ok()
        .map(|fork| fork.as_str().to_owned())
}

#[cfg(test)]
mod tests {
    use std::fs;

    use super::{is_idle, layout_fork};

    #[test]
    fn layout_fork_reads_only_the_canonical_forks_directory() {
        let root = tempfile::Builder::new()
            .prefix("slipfork-layout-fork")
            .tempdir_in("/var/tmp")
            .expect("create a disk temp directory");
        let parent = root.path().join("demo");
        let fork = parent.join(slipfork_world::FORKS_DIR).join("feat-auth");
        fs::create_dir_all(&fork).expect("create the fork checkout");
        assert_eq!(layout_fork(&fork).as_deref(), Some("feat-auth"));
        assert_eq!(layout_fork(&parent), None);
        assert_eq!(layout_fork(&fork.join("nested")), None);
        assert_eq!(
            layout_fork(&parent.join(slipfork_world::FORKS_DIR).join("Bad_Name")),
            None,
            "an invalid fork name never reserves a fork"
        );
    }

    /// A matching head is Idle only with a warm cache and every declared service bundled.
    #[test]
    fn no_cache_never_idles_even_with_a_matching_head() {
        assert!(is_idle(false, true, true), "a warm matching world is Idle");
        assert!(
            !is_idle(true, true, true),
            "`--no-cache` always re-plans and rebuilds"
        );
        assert!(!is_idle(false, false, true), "a changed head reconciles");
        assert!(!is_idle(false, true, false), "a missing bundle reconciles");
    }
}
