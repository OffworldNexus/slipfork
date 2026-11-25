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
use slipfork_world::{LedgerError, RootRequest, RuntimePods, WorldName, WorldRow};

use super::core::{Core, World, ledger_failure};
use crate::daemon::Failure;
use crate::daemon::boot::BootSession;
use crate::daemon::lease::{self, Head};
use crate::daemon::now_ms;
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

/// The supervisor pid of a row, or `0` when it has never been published.
fn pid_of(row: &WorldRow) -> u32 {
    row.supervisor_pid
        .and_then(|pid| u32::try_from(pid).ok())
        .unwrap_or(0)
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
        let Some(World::Live { row, .. }) = self.worlds.get(&key) else {
            return Ok(UpAction::Boot);
        };
        let Some(pods_dir) = row.pods_dir.clone() else {
            return Ok(UpAction::Boot);
        };
        let supervisor_pid = pid_of(row);
        if supervisor_pid == 0 || !pid_alive(supervisor_pid) || !world_answers(&pods_dir) {
            return Ok(UpAction::Boot);
        }
        if is_idle(
            no_cache,
            self.head_matches(&row.checkout_path, &row.world_name, desired),
            plan::services_bundled(&row.checkout_path, &pods_dir),
        ) {
            return Ok(UpAction::Idle(Box::new(Heal {
                world: row.world_name.clone(),
                project: row.checkout_path.clone(),
                pods_dir,
                supervisor_pid,
                started_at_ms: row
                    .started_at_ms
                    .and_then(|ms| u64::try_from(ms).ok())
                    .unwrap_or(0),
                fork: self.fork_name(row.id),
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
        let pods_dir = match self.worlds.get(key) {
            Some(World::Live { row, .. }) => row.pods_dir.clone()?,
            _ => return None,
        };
        let Some(World::Live { row, child }) = self.worlds.remove(key) else {
            return None;
        };
        let supervisor_pid = pid_of(&row);
        self.remove_lease(&row.checkout_path, &row.world_name);
        self.ingress = None;
        Some(Reconcile {
            project: row.checkout_path,
            pods_dir,
            supervisor_pid,
            child,
        })
    }
}

impl Core {
    /// Spawns `__supervise` and leaves a `Booting` entry; the returned session owns the boot.
    ///
    /// `world_id` names the row a fork already reserved; `None` registers (or reuses) the root row
    /// for this checkout. The caller holds one `Core` guard for the reservation, so a fork
    /// snapshot and its boot can never observe two different world maps.
    pub(crate) fn begin_up(
        &mut self,
        params: &Value,
        planned: lease::Planned,
        world_id: Option<i64>,
    ) -> Result<BootSession, Failure> {
        let project = project_path(params)?;
        let key = key_of(&project);
        match self.worlds.get(&key) {
            Some(World::Booting { row, .. }) => {
                return Err(Failure::new(
                    "world-booting",
                    format!("world `{}` is still starting", row.world_name),
                ));
            }
            Some(World::Live { row, .. })
                if pid_of(row) != 0
                    && pid_alive(pid_of(row))
                    && row.pods_dir.as_deref().is_some_and(world_answers) =>
            {
                return Err(Failure::new(
                    "world-running",
                    format!("world `{}` is already running", row.world_name),
                ));
            }
            _ => {}
        }
        if let Some(stale) = self.worlds.remove(&key) {
            stale.reap_if_exited();
        }
        let row = match world_id {
            Some(id) => self.row_by_id(id).ok_or_else(|| {
                Failure::new("no-world", "the world row disappeared from the ledger")
            })?,
            None => match self.row_for_project(&project) {
                Some(row) => row,
                None => self.register_root(&project, params)?,
            },
        };
        self.spawn(project, key, params, planned, row)
    }

    /// Registers this checkout's root row, mapping a taken world name to its one remedy.
    ///
    /// The ledger is the only identity owner: a fresh registration passes `registered == None`
    /// down to [`crate::world::WorldIdentities`], which silently drops a stale
    /// `.slipfork/world.json` instead of adopting its world.
    fn register_root(&self, project: &Path, params: &Value) -> Result<WorldRow, Failure> {
        let world = crate::world::WorldIdentities::new(Some(&self.ledger))
            .name(project, params.get("world").and_then(Value::as_str))
            .map_err(|message| Failure::new("world", message))?;
        let request = RootRequest {
            name: world,
            checkout_path: project.to_path_buf(),
            store_root: self.host.store_root_for(project),
            now_ms: now_ms(),
        };
        self.ledger.register_root(request).map_err(|error| match error {
            LedgerError::NameTaken { world_name } => Failure::new(
                "world-name-taken",
                format!(
                    "world `{world_name}` is already registered in this store\nnext: run `slipfork up --world <other-name>`"
                ),
            ),
            other => ledger_failure(other),
        })
    }

    fn spawn(
        &mut self,
        project: PathBuf,
        key: String,
        params: &Value,
        planned: lease::Planned,
        row: WorldRow,
    ) -> Result<BootSession, Failure> {
        let world = WorldName::parse(&row.world_name).map_err(|error| {
            Failure::new(
                "invalid-world",
                error.to_string().lines().next().unwrap_or_default(),
            )
        })?;
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
        let mut child = command
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
        let supervisor_pid = child.id();
        let stdout = child.stdout.take();
        let store_root = row.store_root.clone();
        let world_name = row.world_name.clone();
        let world_id = row.id;
        self.worlds
            .insert(key.clone(), World::Booting { row, child });
        Ok(BootSession::new(
            key,
            world_id,
            log_path,
            stdout,
            supervisor_pid,
            pods_dir,
            project,
            store_root,
            world_name,
            planned,
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::is_idle;

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
