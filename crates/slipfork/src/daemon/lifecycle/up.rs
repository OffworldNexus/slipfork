//! Spawning `__supervise` and reserving the booting placeholder.

use std::fs::{self, OpenOptions};
use std::hash::{Hash, Hasher};
use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant, UNIX_EPOCH};

use serde_json::Value;
use slipfork_world::control::{PROTOCOL_VERSION, Request, decode_init_frame, encode};

use super::core::{Core, World};
use crate::daemon::Failure;
use crate::daemon::boot::BootSession;
use crate::daemon::registry::{self, Fingerprint};
use crate::daemon::rpc::{key_of, pid_alive, world_answers};
use crate::world::features::WorldFeatures;

/// Longest a single reconcile control call may block (`List`, `Stop`, `Shutdown`).
const RECONCILE_CALL: Duration = Duration::from_secs(10);
/// Longest the old supervisor gets to exit after `Shutdown` before its group is killed.
const RECONCILE_EXIT: Duration = Duration::from_secs(10);

/// What one `up` must do, decided under the daemon lock and executed outside it.
pub(crate) enum UpAction {
    /// The live world's `Forkfile` fingerprint still matches: answer `0ms`.
    Idle(Value),
    /// Detached live world that must be stopped before `begin_up` re-applies the plan.
    Reconcile(Box<Reconcile>),
    /// Nothing live; boot normally.
    Boot,
}

/// A live world detached from [`Core`] so the world calls run without the daemon lock.
pub(crate) struct Reconcile {
    project: PathBuf,
    pods_dir: PathBuf,
    supervisor_pid: u32,
    child: Option<Child>,
}

impl Core {
    /// Decides one idempotent `up` without performing any blocking world call while the core lock
    /// is held. An edited `Forkfile` detaches the live world for [`reconcile`], which stops every
    /// orphan, shuts the world down, and lets `begin_up` re-apply the plan from cache.
    pub(crate) fn up_plan(&mut self, params: &Value) -> Result<UpAction, Failure> {
        let project = PathBuf::from(
            params
                .get("project")
                .and_then(Value::as_str)
                .ok_or_else(|| Failure::new("bad-params", "`project` is required"))?,
        );
        let key = key_of(&project);
        let Some(World::Live { entry, .. }) = self.worlds.get(&key) else {
            return Ok(UpAction::Boot);
        };
        let entry = entry.clone();
        if !pid_alive(entry.supervisor_pid) || !world_answers(&entry.pods_dir) {
            return Ok(UpAction::Boot);
        }
        let current = fingerprint(&project);
        let recorded = registry::load_fingerprints(&self.state_dir)
            .ok()
            .and_then(|mut all| all.remove(&key));
        let unchanged = match (&current, &recorded) {
            (Some(current), Some(recorded)) => current == recorded,
            // A world booted before fingerprints were recorded (or by another binary) still
            // matches when its `Forkfile` predates the boot; otherwise reconcile once.
            (Some(current), None) => {
                let predates = forkfile_predates(&project, entry.started_at_ms);
                if predates {
                    let _ = registry::store_fingerprint(&self.state_dir, &project, Some(current));
                }
                predates
            }
            _ => false,
        };
        if unchanged {
            return Ok(UpAction::Idle(serde_json::json!({
                "world": entry.world,
                "url": format!("http://{}.localhost", entry.world),
                "elapsed_ms": 0,
            })));
        }
        let Some(World::Live { entry, child }) = self.worlds.remove(&key) else {
            return Ok(UpAction::Boot);
        };
        let _ = registry::store_fingerprint(&self.state_dir, &project, None);
        self.ingress = None;
        let _ = self.persist();
        Ok(UpAction::Reconcile(Box::new(Reconcile {
            project: entry.project,
            pods_dir: entry.pods_dir,
            supervisor_pid: entry.supervisor_pid,
            child,
        })))
    }
}

/// Stops every live pod the edited `Forkfile` no longer declares, shuts the detached world down,
/// and reaps it. Runs with **no daemon lock held**, so `ps`/`ping` stay responsive.
pub(crate) fn reconcile(plan: Reconcile) {
    let declared = declared_ids(&plan.project);
    if let Some(declared) = &declared
        && let Some(pods) = call_world(
            &plan.pods_dir,
            &Request::List {
                v: PROTOCOL_VERSION,
                id: "daemon-reconcile-list".to_owned(),
            },
        )
    {
        for pod in slipfork_world::pod_statuses(&pods).unwrap_or_default() {
            if !pod.running || declared.iter().any(|id| id == &pod.id) {
                continue;
            }
            let _ = call_world(
                &plan.pods_dir,
                &Request::Stop {
                    v: PROTOCOL_VERSION,
                    id: format!("daemon-stop-{}", pod.id),
                    pod: pod.id,
                },
            );
        }
    }
    let _ = shut_down(&plan.pods_dir);
    wait_or_kill(plan.supervisor_pid, plan.child, RECONCILE_EXIT);
    let _ = fs::remove_file(plan.project.join(".slipfork/world.json"));
}

/// The ids the current `Forkfile` declares; `None` when it does not compile.
fn declared_ids(project: &Path) -> Option<Vec<String>> {
    let document = fs::read_to_string(project.join("Forkfile"))
        .ok()
        .and_then(|source| slipfork_forkfile::compile(&source).ok())?;
    let features = WorldFeatures::load_resolved(project, &document.document).ok()?;
    Some(
        document
            .document
            .services_with(&features)
            .into_iter()
            .map(|service| service.id.to_owned())
            .collect(),
    )
}

/// One typed control call with a bounded read; `None` on any transport or reply failure.
fn call_world(pods_dir: &Path, request: &Request) -> Option<Value> {
    let path = slipfork_world::control::control_socket_path(pods_dir).ok()?;
    let mut stream = UnixStream::connect(&path).ok()?;
    stream.set_read_timeout(Some(RECONCILE_CALL)).ok()?;
    stream.write_all(&encode(request).ok()?).ok()?;
    stream.flush().ok()?;
    let mut reader = BufReader::new(stream);
    let mut line = String::new();
    reader.read_line(&mut line).ok()?;
    match decode_init_frame(line.trim()).ok()? {
        slipfork_world::control::InitFrame::Reply { ok, error, .. } => match (error, ok) {
            (None, Some(value)) => Some(value),
            _ => None,
        },
        _ => None,
    }
}

/// Sends `Shutdown` and tolerates the init's `Stopped` reply.
fn shut_down(pods_dir: &Path) -> std::io::Result<()> {
    let path = slipfork_world::control::control_socket_path(pods_dir)
        .map_err(|error| std::io::Error::other(error.to_string()))?;
    let mut stream = UnixStream::connect(&path)?;
    stream.set_read_timeout(Some(RECONCILE_CALL))?;
    let request = Request::Shutdown {
        v: PROTOCOL_VERSION,
        id: "daemon-reconcile-down".to_owned(),
    };
    stream
        .write_all(&encode(&request).map_err(|error| std::io::Error::other(error.to_string()))?)?;
    stream.flush()?;
    let mut reader = BufReader::new(stream);
    let mut line = String::new();
    let _ = reader.read_line(&mut line);
    Ok(())
}

/// Waits `budget` for the supervisor to exit, then kills its `-pgid` and reaps it.
fn wait_or_kill(pid: u32, mut child: Option<Child>, budget: Duration) {
    let deadline = Instant::now() + budget;
    loop {
        if let Some(handle) = child.as_mut() {
            match handle.try_wait() {
                Ok(Some(_)) | Err(_) => return,
                Ok(None) => {}
            }
        } else if !pid_alive(pid) {
            return;
        }
        if Instant::now() >= deadline {
            break;
        }
        std::thread::sleep(Duration::from_millis(25));
    }
    if let Some(handle) = child.as_mut() {
        let _ = handle.kill();
        let _ = handle.wait();
    }
    if let Ok(pid) = i32::try_from(pid) {
        slipfork_world::kill_group(pid);
    }
}

impl Core {
    /// Spawns `__supervise` and leaves a `Booting` placeholder; the returned session owns the boot.
    ///
    /// The caller holds one `Core` guard for the reservation, so a fork snapshot and its boot can
    /// never observe two different world maps.
    pub(crate) fn begin_up(&mut self, params: &Value) -> Result<BootSession, Failure> {
        let project = PathBuf::from(
            params
                .get("project")
                .and_then(Value::as_str)
                .ok_or_else(|| Failure::new("bad-params", "`project` is required"))?,
        );
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
        self.spawn(project, key, params)
    }

    fn spawn(
        &mut self,
        project: PathBuf,
        key: String,
        params: &Value,
    ) -> Result<BootSession, Failure> {
        let requested = params.get("world").and_then(Value::as_str);
        let world = crate::world::world_name(&project, requested)
            .map_err(|message| Failure::new("invalid-world", message))?;
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
        let child = Command::new(&self.host.exe)
            .arg("__supervise")
            .arg("--project")
            .arg(&project)
            .arg("--name")
            .arg(world.as_str())
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
                supervisor_pid: child.id(),
            },
        );
        let mut session = BootSession::new(key, log_path, child, project.clone());
        if let Some(fork) = layout_fork(&project) {
            session.reserve_fork(fork);
        }
        Ok(session)
    }
}

/// The fork name when `project` is the canonical `<parent>/.forks/<fork>` checkout.
///
/// `up <fork>` re-boots an existing checkout through [`Core::begin_up`], so the published registry
/// entry must still carry the fork name for `ps` to nest it under its parent world.
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

/// The compiled `Forkfile`'s stable content digest plus its resolved active features.
fn fingerprint(project: &Path) -> Option<Fingerprint> {
    let source = fs::read_to_string(project.join("Forkfile")).ok()?;
    let checked = slipfork_forkfile::compile(&source).ok()?;
    let config = WorldFeatures::load(project).ok()?;
    let features: Vec<String> = config
        .resolve(&checked.document)
        .ok()?
        .active()
        .iter()
        .cloned()
        .collect();
    let raw = fs::read(WorldFeatures::path(project)).ok()?;
    let edits: Vec<String> = config.edits.iter().map(ToString::to_string).collect();
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    source.hash(&mut hasher);
    raw.hash(&mut hasher);
    config.no_default.hash(&mut hasher);
    edits.hash(&mut hasher);
    features.hash(&mut hasher);
    Some(Fingerprint {
        digest: format!("{:016x}", hasher.finish()),
        features,
    })
}

/// Whether the `Forkfile` was last modified at or before the world became ready.
fn forkfile_predates(project: &Path, started_at_ms: u64) -> bool {
    if started_at_ms == 0 {
        return false;
    }
    fs::metadata(project.join("Forkfile"))
        .and_then(|metadata| metadata.modified())
        .ok()
        .and_then(|modified| modified.duration_since(UNIX_EPOCH).ok())
        .map(|since| since.as_millis() as u64 <= started_at_ms)
        .unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use std::fs;

    use super::layout_fork;

    fn temp(prefix: &str) -> tempfile::TempDir {
        tempfile::Builder::new()
            .prefix(prefix)
            .tempdir_in("/var/tmp")
            .expect("create a disk temp directory")
    }

    #[test]
    fn layout_fork_reads_only_the_canonical_forks_directory() {
        let root = temp("slipfork-layout-fork");
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
}
