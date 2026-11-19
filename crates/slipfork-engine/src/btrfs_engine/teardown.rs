//! The one teardown order: stop supervised service trees, reap the world PID 1, drop the control
//! directory, destroy the world subvolume (`ADR-0010` §3, `ADR-0009` §3).

#![forbid(unsafe_code)]

use std::fs;
use std::sync::atomic::{AtomicU8, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use slipfork_world::{EngineError, ServiceLogs, ServiceState, WorldHandle};

use crate::supervise::SupervisedProcess;
use crate::wait::D_STATE_KILL_GRACE;

use super::services::{RuntimeKind, ServiceProcess, ServiceRecord};
use super::{BtrfsEngine, WorldSubvolume, io_error};

/// Grace period between `SIGTERM` and `SIGKILL` when stopping a service.
pub(crate) const SERVICE_STOP_GRACE: Duration = Duration::from_secs(5);
/// Grace period for the world PID 1 to exit after closing its `stdin` lifeline / `SIGTERM`.
pub(crate) const WORLD_STOP_TERM_GRACE: Duration = Duration::from_secs(5);
/// Brief `stdin`-EOF window during [`BtrfsEngine`]'s `Drop` before escalating to `SIGKILL`.
pub(crate) const WORLD_DROP_TERM_GRACE: Duration = Duration::from_millis(100);
/// Grace period for the world `unshare` launcher and PID namespace to disappear after `SIGKILL`.
pub(crate) const WORLD_STOP_KILL_GRACE: Duration = Duration::from_secs(10);

/// One service's teardown handles, taken out of the world record before `destroy_world` releases
/// the world's processes and storage.
pub(crate) struct ServiceTeardown {
    pub(crate) runtime_kind: RuntimeKind,
    pub(crate) id: String,
    pub(crate) state: Arc<AtomicU8>,
    pub(crate) log: ServiceLogs,
    pub(crate) process: Arc<Mutex<ServiceProcess>>,
}

impl ServiceTeardown {
    pub(crate) fn from_record(id: &str, service: &ServiceRecord) -> Self {
        Self {
            runtime_kind: service.runtime_kind,
            id: id.to_owned(),
            state: Arc::clone(&service.state),
            log: service.log.clone(),
            process: Arc::clone(&service.process),
        }
    }
}

impl BtrfsEngine {
    /// Stop `services` in `ADR-0009` §3 order: app components first, then backing-service
    /// containers, so no app request observes a half-shut-down database.
    pub(crate) fn stop_services_with_grace(
        &self,
        services: &[ServiceTeardown],
        grace: Duration,
    ) -> Result<(), EngineError> {
        for kind in [RuntimeKind::WorldProcess, RuntimeKind::OciContainer] {
            for service in services
                .iter()
                .filter(|service| service.runtime_kind == kind)
            {
                self.stop_service_process(
                    &service.id,
                    &service.state,
                    &service.log,
                    &service.process,
                    grace,
                )?;
            }
        }
        Ok(())
    }

    pub(crate) fn stop_service_inner(
        &self,
        world: &WorldHandle,
        id: &str,
    ) -> Result<(), EngineError> {
        let (state, log, process) = {
            let mut worlds = self.lock()?;
            let record = self.live_record(&mut worlds, world)?;
            let service = record
                .services
                .get(id)
                .ok_or_else(|| EngineError::UnknownService {
                    world: format!("{world:?}"),
                    id: id.to_owned(),
                })?;
            (
                Arc::clone(&service.state),
                service.log.clone(),
                Arc::clone(&service.process),
            )
        };
        self.stop_service_process(id, &state, &log, &process, SERVICE_STOP_GRACE)
    }

    /// Stop one service's process tree with a bounded `SIGTERM` → `SIGKILL` grace.
    ///
    /// The one stop implementation: `grace` is the `SIGTERM` window (`Duration::ZERO` for a spawn
    /// that already failed, which escalates to `SIGKILL` immediately). A world process is
    /// signalled as its whole **process group**, so the `sh -eu -c` wrapper and the service it
    /// forked are both stopped — including a descendant that outlived the supervised child.
    /// Idempotent: a service that already exited on its own, or was stopped before, keeps its
    /// retained logs and reports success once no group member survives. Takes the record's handles
    /// directly so world teardown can stop services after the world has been fenced.
    pub(crate) fn stop_service_process(
        &self,
        id: &str,
        state: &Arc<AtomicU8>,
        log: &ServiceLogs,
        process: &Arc<Mutex<ServiceProcess>>,
        grace: Duration,
    ) -> Result<(), EngineError> {
        let running =
            ServiceState::from_bits(state.load(Ordering::Relaxed)) == ServiceState::Running;
        let (mut supervisor, process_group, container) = {
            let Ok(mut process) = process.lock() else {
                return Err(EngineError::Execution(format!(
                    "service '{id}' process state is poisoned"
                )));
            };
            (
                process.supervisor.take(),
                process.process_group.clone(),
                process.container.clone(),
            )
        };
        // A row that already left `Running` still owns its group: the supervised child may have
        // exited while a descendant kept running (the `sh`/`python3` shape), so the early return
        // only applies when there is nothing left to reap.
        if !running && supervisor.is_none() && process_group.is_none() && container.is_none() {
            log.close();
            return Ok(());
        }

        if let Some(container) = &container {
            // A backing-service container is signalled through `crun` (not a host process group)
            // and its supervisor is the `nsenter` launcher.
            let mut supervised = SupervisedProcess::from_parts(supervisor.take(), None);
            if grace > Duration::ZERO {
                // A container that already exited refuses `crun kill TERM`; the supervisor exit
                // observed below is the authority, so the refusal is not an error of the stop.
                let _ = container.runtime.signal(&container.id, "TERM");
            }
            let deadline = Instant::now() + grace;
            let mut stopped = grace > Duration::ZERO;
            if stopped {
                stopped = supervised.wait_exit(deadline.saturating_duration_since(Instant::now()));
            }
            if !stopped {
                let killed = container.runtime.signal(&container.id, "KILL");
                if let Err(err) =
                    supervised.reap_child(SERVICE_STOP_GRACE, &format!("service '{id}'"))
                {
                    // The supervisor is restored so a retry can still signal it, and the service
                    // stays `Running` with its log open: marking it `Stopped` here would make a
                    // retried `teardown_world` short-circuit at the "not Running" guard above
                    // while the supervisor still lives.
                    if let Some(child) = supervised.into_child()
                        && let Ok(mut guard) = process.lock()
                    {
                        guard.supervisor = Some(child);
                    }
                    return finish_service_stop(state, log, Err(err));
                }
                // The supervisor is reaped; a refused `crun kill` is the container's own anomaly.
                killed?;
            }
            container.runtime.delete(&container.id)?;
            let _ = fs::remove_dir_all(&container.bundle_host);
            if let Ok(mut guard) = process.lock() {
                guard.process_group = None;
                // The bundle is gone, so a repeated stop sees nothing left to signal.
                guard.container = None;
            }
            return finish_service_stop(state, log, Ok(()));
        }

        // A world process is only recorded after a successful group pin:
        // `spawn_world_process` stops and fails closed otherwise, so a row without a group has no
        // honest signal target and must never be reported stopped.
        let Some(group) = process_group else {
            if let Some(child) = supervisor.take()
                && let Ok(mut guard) = process.lock()
            {
                guard.supervisor = Some(child);
            }
            return finish_service_stop(
                state,
                log,
                Err(EngineError::Execution(format!(
                    "service '{id}' has no pinned process group; spawn_world_process records a \
                     group or fails closed rather than leave an unsignalable world process"
                ))),
            );
        };
        let label = format!("service '{id}' process group {}", group.pid);
        let mut supervised = SupervisedProcess::from_parts(supervisor.take(), Some(group));
        if let Err(err) = supervised.stop(grace, SERVICE_STOP_GRACE, D_STATE_KILL_GRACE, &label) {
            // The supervisor is restored so a retry can still signal it, and the service stays
            // `Running` with its log open (see the container branch above).
            if let Some(child) = supervised.into_child()
                && let Ok(mut guard) = process.lock()
            {
                guard.supervisor = Some(child);
            }
            return finish_service_stop(state, log, Err(err));
        }
        // Nothing in the group survives: drop the handle so a repeated stop (idempotent by
        // contract) never signals a host pgid the kernel may have recycled for a new process.
        if let Ok(mut guard) = process.lock() {
            guard.process_group = None;
        }
        finish_service_stop(state, log, Ok(()))
    }

    pub(crate) fn restore_launcher(&self, world: &WorldHandle, launcher: std::process::Child) {
        if let Ok(mut worlds) = self.lock()
            && let Ok(record) = self.owned(&mut worlds, world)
        {
            record.launcher = Some(launcher);
        }
    }

    /// Shut down the persistent world PID 1 over its `stdin` lifeline pipe and reap its `unshare`
    /// launcher, escalating to `SIGKILL` after `term_grace`.
    ///
    /// PID 1 of a child PID namespace carries `SIGNAL_UNKILLABLE` (`pid_namespaces(7)`): the
    /// kernel silently drops unhandled `SIGTERM` from an ancestor namespace, whereas `SIGKILL` is
    /// forcibly delivered. Closing `launcher.stdin` makes `slipfork-world-init` observe `EOF` and
    /// exit `0` voluntarily so `unshare` reaps it immediately; `SIGTERM` is also sent (after
    /// `/proc/<pid>/ns/pid` identity verification) for un-namespaced unit-test doubles. The stop
    /// itself is [`SupervisedProcess::stop`] — the same one path a service group takes — with this
    /// world-init grace policy: `term_grace`, then `WORLD_STOP_KILL_GRACE`, no `D`-state retry
    /// budget. Any `kill` or bounded-wait failure restores `launcher` into the record and returns
    /// a typed [`EngineError::Execution`] naming the survivor before storage is touched.
    pub(crate) fn reap_world_init(
        &self,
        world: &WorldHandle,
        term_grace: Duration,
    ) -> Result<(), EngineError> {
        let (launcher, init_pid, init_identity) = {
            let mut worlds = self.lock()?;
            let record = self.owned(&mut worlds, world)?;
            (
                record.launcher.take(),
                record.init_pid,
                record.init_identity.clone(),
            )
        };
        if launcher.is_none() && init_pid <= 1 && init_identity.is_none() {
            return Ok(());
        }
        if launcher.is_none() && init_identity.is_none() {
            return Err(EngineError::Execution(format!(
                "cannot reap world init pid {init_pid}: missing /proc/{init_pid}/ns/pid identity"
            )));
        }

        let mut supervised = SupervisedProcess::from_parts(launcher, init_identity);
        supervised.close_stdin();
        let label = format!("world init pid {init_pid}");
        if let Err(err) = supervised.stop(term_grace, WORLD_STOP_KILL_GRACE, Duration::ZERO, &label)
        {
            if let Some(child) = supervised.into_child() {
                self.restore_launcher(world, child);
            }
            return Err(err);
        }

        let mut worlds = self.lock()?;
        let record = self.owned(&mut worlds, world)?;
        record.init_pid = 0;
        record.init_identity = None;
        Ok(())
    }

    /// Tear down one world's services, persistent PID 1 + `unshare` launcher, control directory,
    /// and world subvolume in `ADR-0010` §3 order, removing the world record once complete.
    ///
    /// Shared by [`WorldEngine::destroy_world`] and [`BtrfsEngine`]'s `Drop`: any failed process or
    /// storage step aborts before removing the record so a retried `destroy_world` resumes where it
    /// stopped.
    pub(crate) fn teardown_world(
        &self,
        world: &WorldHandle,
        service_grace: Duration,
        init_term_grace: Duration,
    ) -> Result<(), EngineError> {
        let (world_id, ctl_dir, has_subvolume, services) = {
            let mut worlds = self.lock()?;
            let record = self.owned(&mut worlds, world)?;
            // Fence the record: every later operation on this handle fails closed with `WorldGone`
            // while teardown releases the world's processes and storage. A record already fenced is
            // re-entered, so a failed teardown is always retryable instead of leaking the world.
            record.destroying = true;
            let services: Vec<ServiceTeardown> = record
                .services
                .iter()
                .map(|(id, service)| ServiceTeardown::from_record(id, service))
                .collect();
            (
                record.world_id.clone(),
                record.ctl_dir.clone(),
                matches!(record.subvolume, WorldSubvolume::Cloned(_)),
                services,
            )
        };

        // 1. Stop services (app components first, then backing-service containers).
        self.stop_services_with_grace(&services, service_grace)?;
        // 2. Reap the persistent PID 1 (and with it the whole PID namespace) and its launcher.
        self.reap_world_init(world, init_term_grace)?;
        // 3. Drop the ephemeral control directory (sockets, config, OCI runtime state).
        if ctl_dir.exists() {
            fs::remove_dir_all(&ctl_dir).map_err(|err| io_error(&ctl_dir, &err))?;
        }
        // 4. Destroy the world subvolume when present; the driver clears `BTRFS_SUBVOL_RDONLY` first.
        if has_subvolume {
            self.driver
                .destroy_subvolume(&self.worlds_dir(), &world_id)?;
        }

        // The world subvolume is gone, so the record must go too: a long-lived daemon creates and
        // destroys worlds forever, and the old handle now resolves to `EngineError::WorldGone`
        // exactly like every other operation on a destroyed world.
        self.lock()?.remove(&world.seq());
        Ok(())
    }
}

/// Apply the terminal state of one service-stop attempt.
///
/// Only a fully reaped supervisor becomes [`ServiceState::Stopped`] with a closed log; a failed
/// `kill`/reap keeps the service [`ServiceState::Running`] and its log open so a retried teardown
/// re-enters `kill_and_reap_child` instead of short-circuiting on `Stopped` while a supervisor
/// process still lives.
fn finish_service_stop(
    state: &Arc<AtomicU8>,
    log: &ServiceLogs,
    outcome: Result<(), EngineError>,
) -> Result<(), EngineError> {
    if outcome.is_ok() {
        state.store(ServiceState::Stopped.to_bits(), Ordering::Relaxed);
        log.close();
    }
    outcome
}
