//! Spawning `__supervise` and reserving the booting placeholder.

use std::fs::{self, OpenOptions};
use std::hash::{Hash, Hasher};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant, UNIX_EPOCH};

use serde_json::{Value, json};
use slipfork_world::RuntimePods;
use slipfork_world::control::{PROTOCOL_VERSION, Request};

use super::core::{Core, World};
use crate::daemon::Failure;
use crate::daemon::boot::BootSession;
use crate::daemon::registry::{self, Fingerprint};
use crate::daemon::rpc::{key_of, pid_alive, world_answers, world_call_within};
use crate::ps::PsService;
use crate::world::features::WorldFeatures;

/// Longest a single reconcile control call may block (`List`, `Stop`, `Shutdown`).
const RECONCILE_CALL: Duration = Duration::from_secs(10);
/// Longest the old supervisor gets to exit after `Shutdown` before its group is killed.
const RECONCILE_EXIT: Duration = Duration::from_secs(10);
const REAP_BUDGET: Duration = Duration::from_secs(5);
const HEAL_INSPECT: Duration = Duration::from_secs(2);
const HEAL_RESTART: Duration = Duration::from_secs(30);
const HEAL_SETTLE: Duration = Duration::from_millis(250);
const HEAL_POLL: Duration = Duration::from_millis(50);
const HEAL_VERIFY: Duration = Duration::from_millis(1500);

/// What one `up` must do, decided under the daemon lock and executed outside it.
pub(crate) enum UpAction {
    /// The live world's fingerprint still matches: verify/restart its pods outside the lock.
    Heal(Box<Heal>),
    /// Detached live world that must be stopped before `begin_up` re-applies the plan.
    Reconcile(Box<Reconcile>),
    /// Nothing live; boot normally.
    Boot,
}

/// An unchanged world detached from [`Core`] so its health check never holds the daemon lock.
pub(crate) struct Heal {
    world: String,
    project: PathBuf,
    pods_dir: PathBuf,
    supervisor_pid: u32,
    started_at_ms: u64,
    fork: Option<String>,
}

/// A live world detached from [`Core`] so the world calls run without the daemon lock.
pub(crate) struct Reconcile {
    project: PathBuf,
    pods_dir: PathBuf,
    supervisor_pid: u32,
    child: Option<Child>,
}

/// The `project` parameter every lifecycle request carries.
pub(crate) fn project_path(params: &Value) -> Result<PathBuf, Failure> {
    params
        .get("project")
        .and_then(Value::as_str)
        .map(PathBuf::from)
        .ok_or_else(|| Failure::new("bad-params", "`project` is required"))
}

/// The current head digest for `params`, computed **before** the daemon lock is taken so a large
/// build-context walk never blocks `ps`/`ping`.
pub(crate) fn fingerprint_for(params: &Value) -> Result<Option<Fingerprint>, Failure> {
    Ok(fingerprint(&project_path(params)?))
}

impl Core {
    /// Decides one idempotent `up`: unchanged heals outside the lock; edited reconciles then boots.
    pub(crate) fn up_plan(
        &mut self,
        params: &Value,
        current: Option<Fingerprint>,
    ) -> Result<UpAction, Failure> {
        let project = project_path(params)?;
        let key = key_of(&project);
        let Some(World::Live { entry, .. }) = self.worlds.get(&key) else {
            return Ok(UpAction::Boot);
        };
        let entry = entry.clone();
        if !pid_alive(entry.supervisor_pid) || !world_answers(&entry.pods_dir) {
            return Ok(UpAction::Boot);
        }
        // Only heal when the recorded head still matches disk AND every declared service already
        // wrote its bundle; a half-booted or edited world reconciles to re-run the full DAG.
        if !self.world_stale(&project, current, entry.started_at_ms)
            && services_bundled(&project, &entry.pods_dir)
        {
            return Ok(UpAction::Heal(Box::new(Heal {
                world: entry.world,
                project: entry.project,
                pods_dir: entry.pods_dir,
                supervisor_pid: entry.supervisor_pid,
                started_at_ms: entry.started_at_ms,
                fork: entry.fork,
            })));
        }
        let Some(plan) = self.detach_live(&key) else {
            return Ok(UpAction::Boot);
        };
        Ok(UpAction::Reconcile(Box::new(plan)))
    }

    /// Whether the live world's recorded head (`Forkfile` + every image's `Dockerfile` + context)
    /// differs from `current`. A world booted before fingerprints were recorded still matches when
    /// its inputs predate the boot; otherwise it reconciles once and records the current head.
    pub(crate) fn world_stale(
        &self,
        project: &Path,
        current: Option<Fingerprint>,
        started_at_ms: u64,
    ) -> bool {
        let recorded = registry::load_fingerprints(&self.state_dir)
            .ok()
            .and_then(|mut all| all.remove(&key_of(project)));
        match (&current, &recorded) {
            (Some(current), Some(recorded)) => current != recorded,
            (Some(current), None) => {
                let predates = inputs_predate(project, started_at_ms);
                if predates {
                    let _ = registry::store_fingerprint(&self.state_dir, project, Some(current));
                }
                !predates
            }
            _ => true,
        }
    }

    /// Detaches the live world so `reconcile` and the reboot run outside the daemon lock.
    pub(crate) fn detach_live(&mut self, key: &str) -> Option<Reconcile> {
        let Some(World::Live { entry, child }) = self.worlds.remove(key) else {
            return None;
        };
        let _ = registry::store_fingerprint(&self.state_dir, &entry.project, None);
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

pub(crate) fn heal(plan: Heal, emit: &mut dyn FnMut(Value)) -> Result<Value, Failure> {
    let started = Instant::now();
    let mut latest = down_rows(&plan, "daemon-up-health", HEAL_INSPECT)?;
    if latest.is_empty() {
        return Ok(ready(&plan.world, 0));
    }
    let label = format!("Verifying {}", plan.world);
    emit(json!({"event":"started","step":1,"total":1,"kind":"world","label":label}));
    let mut restart_error = None;
    for row in &latest {
        let request = Request::Restart {
            v: PROTOCOL_VERSION,
            id: format!("daemon-up-restart-{}", row.id),
            service: Some(row.id.clone()),
        };
        if let Err(error) = world_call_within(&plan.pods_dir, &request, HEAL_RESTART) {
            restart_error
                .get_or_insert_with(|| error.lines().next().unwrap_or("restart failed").to_owned());
        }
    }
    let deadline = Instant::now() + HEAL_VERIFY;
    let mut last_error = None;
    let mut healthy_since = None;
    while Instant::now() < deadline {
        match down_rows(&plan, "daemon-up-recheck", HEAL_INSPECT) {
            Ok(rows) if rows.is_empty() => {
                let since = *healthy_since.get_or_insert_with(Instant::now);
                if since.elapsed() >= HEAL_SETTLE {
                    let ms = u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX);
                    emit(
                        json!({"event":"finished","step":1,"total":1,"kind":"world","name":plan.world,"state":"running","label":format!("Recovered {}", plan.world),"detail":"services running","elapsed_ms":ms,"duration_ms":ms}),
                    );
                    return Ok(ready(&plan.world, ms));
                }
            }
            Ok(rows) => (latest, healthy_since) = (rows, None),
            Err(error) => (last_error, healthy_since) = (Some(error.message), None),
        }
        std::thread::sleep(HEAL_POLL);
    }
    let id = &latest.first().expect("heal keeps the initial down row").id;
    let detail = restart_error
        .or(last_error)
        .and_then(|error| error.lines().next().map(|line| format!(": {line}")))
        .unwrap_or_default();
    Err(Failure::new(
        "world-unhealthy",
        format!("service `{id}` is not running{detail}\nnext: slipfork logs {id}"),
    ))
}

fn ready(world: &str, elapsed_ms: u64) -> Value {
    json!({ "world": world, "url": format!("http://{world}.localhost"), "elapsed_ms": elapsed_ms })
}

fn down_rows(plan: &Heal, id: &str, timeout: Duration) -> Result<Vec<PsService>, Failure> {
    let request = Request::List {
        v: PROTOCOL_VERSION,
        id: id.to_owned(),
    };
    let reply = world_call_within(&plan.pods_dir, &request, timeout)
        .map_err(|error| world_failure(&plan.world, &error))?;
    let pods = slipfork_world::pod_statuses(&reply)
        .map_err(|error| world_failure(&plan.world, &error.to_string()))?;
    Ok(super::ps::WorldView::new(
        &plan.project,
        &plan.world,
        plan.fork.as_deref(),
        plan.started_at_ms,
        plan.supervisor_pid,
        true,
        false,
        &pods,
    )
    .service_rows()
    .iter()
    .filter(|row| !row.orphaned && !row.running)
    .cloned()
    .collect())
}

fn world_failure(world: &str, error: &str) -> Failure {
    let message = format!("cannot inspect `{world}`: {error}\nnext: slipfork status");
    Failure::new("world-unhealthy", message)
}

/// Stops undeclared pods, shuts the detached world down, and reaps it — with no daemon lock held.
pub(crate) fn reconcile(plan: Reconcile) {
    let declared = declared_ids(&plan.project);
    if let Some(declared) = &declared
        && let Ok(pods) = world_call_within(
            &plan.pods_dir,
            &Request::List {
                v: PROTOCOL_VERSION,
                id: "daemon-reconcile-list".to_owned(),
            },
            RECONCILE_CALL,
        )
    {
        for pod in slipfork_world::pod_statuses(&pods).unwrap_or_default() {
            if !pod.running || declared.iter().any(|id| id == &pod.id) {
                continue;
            }
            let _ = world_call_within(
                &plan.pods_dir,
                &Request::Stop {
                    v: PROTOCOL_VERSION,
                    id: format!("daemon-stop-{}", pod.id),
                    pod: pod.id,
                },
                RECONCILE_CALL,
            );
        }
    }
    let _ = world_call_within(
        &plan.pods_dir,
        &Request::Shutdown {
            v: PROTOCOL_VERSION,
            id: "daemon-reconcile-down".to_owned(),
        },
        RECONCILE_CALL,
    );
    wait_or_kill(
        plan.supervisor_pid,
        &plan.pods_dir,
        plan.child,
        RECONCILE_EXIT,
    );
    let _ = fs::remove_file(plan.project.join(".slipfork/world.json"));
}

fn services_bundled(project: &Path, pods_dir: &Path) -> bool {
    declared_ids(project).is_some_and(|ids| {
        ids.iter()
            .all(|id| pods_dir.join(id).join("config.json").is_file())
    })
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

/// Waits `budget` for the supervisor to exit, then kills and reaps its full world process tree.
fn wait_or_kill(pid: u32, pods_dir: &Path, mut child: Option<Child>, budget: Duration) {
    let deadline = Instant::now() + budget;
    loop {
        if let Some(handle) = child.as_mut() {
            match handle.try_wait() {
                Ok(Some(_)) | Err(_) => break,
                Ok(None) => {}
            }
        } else if !pid_alive(pid) {
            break;
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
    slipfork_world::reap_world_tree(pid, Some(pods_dir), REAP_BUDGET);
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
                pods_dir: pods_dir.clone(),
                supervisor_pid: child.id(),
            },
        );
        let mut session = BootSession::new(key, log_path, child, pods_dir, project.clone());
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

/// The compiled `Forkfile`'s stable content digest, its resolved active features, and every build
/// image's canonical `Dockerfile` + context digest.
pub(crate) fn fingerprint(project: &Path) -> Option<Fingerprint> {
    let source = fs::read_to_string(project.join("Forkfile")).ok()?;
    let checked = slipfork_forkfile::compile(&source).ok()?;
    let config = WorldFeatures::load(project).ok()?;
    let resolved = config.resolve(&checked.document).ok()?;
    let features: Vec<String> = resolved.active().iter().cloned().collect();
    let plan = slipfork_world::world_plan_with(&checked.document, project, &resolved).ok()?;
    let inputs = crate::world::image_inputs(project, &checked.document, &plan.images, &resolved);
    let raw = fs::read(WorldFeatures::path(project)).unwrap_or_default();
    let edits: Vec<String> = config.edits.iter().map(ToString::to_string).collect();
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    source.hash(&mut hasher);
    raw.hash(&mut hasher);
    config.no_default.hash(&mut hasher);
    edits.hash(&mut hasher);
    features.hash(&mut hasher);
    for image in &plan.images {
        image.id.hash(&mut hasher);
        inputs
            .get(&image.id)
            .map(String::as_str)
            .unwrap_or_default()
            .hash(&mut hasher);
    }
    Some(Fingerprint {
        digest: format!("{:016x}", hasher.finish()),
        features,
    })
}

/// Whether `Forkfile`, any image's `Dockerfile`, and their optional feature/ignore files all
/// predate `started_at_ms`.
fn inputs_predate(project: &Path, started_at_ms: u64) -> bool {
    if started_at_ms == 0 {
        return false;
    }
    if !file_predates(&project.join("Forkfile"), started_at_ms) {
        return false;
    }
    let features_path = WorldFeatures::path(project);
    if features_path.exists() && !file_predates(&features_path, started_at_ms) {
        return false;
    }
    let Ok(source) = fs::read_to_string(project.join("Forkfile")) else {
        return false;
    };
    let Ok(checked) = slipfork_forkfile::compile(&source) else {
        return false;
    };
    let Ok(features) = WorldFeatures::load_resolved(project, &checked.document) else {
        return false;
    };
    let Ok(plan) = slipfork_world::world_plan_with(&checked.document, project, &features) else {
        return false;
    };
    plan.images
        .iter()
        .filter(|image| image.from.is_none())
        .all(|image| {
            let paths = image.paths(project);
            let ignore = paths.context.join(".dockerignore");
            file_predates(&paths.dockerfile, started_at_ms)
                && (!ignore.exists() || file_predates(&ignore, started_at_ms))
        })
}

fn file_predates(path: &Path, started_at_ms: u64) -> bool {
    fs::metadata(path)
        .and_then(|metadata| metadata.modified())
        .ok()
        .and_then(|modified| modified.duration_since(UNIX_EPOCH).ok())
        .map(|since| since.as_millis() as u64 <= started_at_ms)
        .unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use std::fs;

    use super::{fingerprint, layout_fork};

    const FORKFILE: &str = "# forkfile: 0.1\nimage app . {\n  dockerfile Dockerfile\n  service web {\n    run: sleep 3600\n  }\n}\n";

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

    /// `up` compares the head it recorded at boot against the `Forkfile`, every `Dockerfile`, and
    /// the copied context, so each edit reconciles and an unchanged tree stays cached.
    #[test]
    fn the_fingerprint_tracks_the_forkfile_dockerfile_and_copied_context() {
        let root = temp("slipfork-up-fingerprint");
        let project = root.path();
        fs::write(project.join("Forkfile"), FORKFILE).expect("write Forkfile");
        let dockerfile = project.join("Dockerfile");
        fs::write(
            &dockerfile,
            "FROM alpine:3.20\nCOPY seed.txt /seed.txt\nRUN echo one\n",
        )
        .expect("write Dockerfile");
        fs::write(project.join("seed.txt"), "one").expect("write the copied context");

        let first = fingerprint(project).expect("fingerprint the project");
        assert_eq!(
            first,
            fingerprint(project).expect("re-fingerprint"),
            "an unchanged tree keeps its head"
        );

        fs::write(
            &dockerfile,
            "FROM alpine:3.20\nCOPY seed.txt /seed.txt\nRUN echo two\n",
        )
        .expect("edit the Dockerfile");
        let dockerfile_edited = fingerprint(project).expect("fingerprint after the edit");
        assert_ne!(
            first, dockerfile_edited,
            "a `RUN` edit must change the head and force a rebuild"
        );

        fs::write(project.join("seed.txt"), "two").expect("edit the copied context");
        let context_edited = fingerprint(project).expect("fingerprint after the context edit");
        assert_ne!(
            dockerfile_edited, context_edited,
            "a copied context edit must change the head and force a rebuild"
        );

        fs::write(
            project.join("Forkfile"),
            format!("{FORKFILE}\nimage worker . {{\n  dockerfile Dockerfile\n}}\n"),
        )
        .expect("add a service");
        assert_ne!(
            context_edited,
            fingerprint(project).expect("fingerprint after the Forkfile edit"),
            "a Forkfile edit must change the head and force a reconcile"
        );
    }
}
