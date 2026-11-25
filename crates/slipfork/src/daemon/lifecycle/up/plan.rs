//! Reconcile ordering: stop undeclared pods, shut the old world down, reap its `-pgid` tree.
//!
//! Everything here runs with zero daemon locks held, after the daemon sealed the new heads; a
//! failed build never reaches this module, so the old world keeps serving.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Child;
use std::time::{Duration, Instant};

use slipfork_world::control::{PROTOCOL_VERSION, Request};

use crate::daemon::rpc::{pid_alive, world_call_within};
use crate::world::features::WorldFeatures;

/// Longest a single reconcile control call may block (`List`, `Stop`, `Shutdown`).
const RECONCILE_CALL: Duration = Duration::from_secs(10);
/// Longest the old supervisor gets to exit after `Shutdown` before its group is killed.
const RECONCILE_EXIT: Duration = Duration::from_secs(10);
const REAP_BUDGET: Duration = Duration::from_secs(5);

/// A live world detached from `Core` so its world calls run without the daemon lock.
pub(crate) struct Reconcile {
    pub(super) project: PathBuf,
    pub(super) pods_dir: PathBuf,
    pub(super) supervisor_pid: u32,
    pub(super) child: Option<Child>,
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

/// Whether every service the current `Forkfile` declares already wrote its pod bundle.
pub(super) fn services_bundled(project: &Path, pods_dir: &Path) -> bool {
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
