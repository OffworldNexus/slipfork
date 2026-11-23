//! Hidden detached-supervisor mode: boot the world, stream typed events, wait for the init.

use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::{Arc, Mutex};

use serde_json::{Value, json};
use slipfork_world::{
    BootEvent, BootFailure, BootScheduler, BootState, BootStep, ResourceKind, StampStore,
    StepOutcome, WorldName, WorldStamp, WorldStampStore, control, is_btrfs_path, plan_epoch,
    run_log::RunCapture, world_plan_with, write_pg_checkpoints,
};

use crate::commands::Session;
use crate::world::features::WorldFeatures;

use super::{HostEnv, SuperviseArgs, WorldState, fail, runtime::WorldRuntime, state_path};

impl Session {
    /// Hidden detached-supervisor mode: boot, wait until `down`, then exit.
    pub fn supervise(&self, args: &SuperviseArgs) -> ExitCode {
        // Detach from the terminal's process group so closing the shell cannot kill the world.
        // SAFETY: `setsid(2)` only moves this process into a new session.
        unsafe {
            libc::setsid();
        }
        match supervisor_main(&args.project, &args.world, self.host()) {
            Ok(()) => ExitCode::SUCCESS,
            Err(message) => fail(message, "inspect `.slipfork/supervisor.log`"),
        }
    }
}

fn supervisor_main(project: &Path, world: &str, env: &HostEnv) -> Result<(), String> {
    let world = WorldName::parse(world).map_err(|error| error.to_string())?;
    let source = fs::read_to_string(project.join("Forkfile"))
        .map_err(|error| format!("read Forkfile: {error}"))?;
    let checked = slipfork_forkfile::compile(&source).map_err(|diagnostics| {
        diagnostics
            .first()
            .map_or("invalid Forkfile".to_owned(), |item| item.message.clone())
    })?;
    let doc: slipfork_forkfile::ast::Forkfile = *checked.document;
    if !is_btrfs_path(project).unwrap_or(false) {
        return Err(format!(
            "`{}` is not on Btrfs\nnext: move the project onto a Btrfs mount",
            project.display()
        ));
    }
    let features = WorldFeatures::load_resolved(project, &doc)
        .map_err(|error| format!("{error}\nnext: {}", WorldFeatures::REMEDY))?;
    let plan = world_plan_with(&doc, project, &features).map_err(|error| error.to_string())?;
    let emitter = Arc::new(Mutex::new(Emitter { open: true }));
    let runtime = Arc::new(WorldRuntime::new(
        project,
        world.clone(),
        &doc,
        &plan,
        features.clone(),
        env,
        Arc::clone(&emitter),
    )?);
    let pods_dir = runtime.pods_dir().to_path_buf();
    let socket = control::control_socket_path(&pods_dir).map_err(|error| error.to_string())?;
    let world_dir = runtime.world_dir();
    write_state(project, &world, &socket)?;

    // Epoch-paired stamps gate every conditional skip: `fork` snapshots the world and each typed
    // PG sibling together, so matching pairs reuse `@<tool>` and `before-start`/`pre-deploy`
    // digests in 0ms while any stale sibling or modified plan forces the hooks to run again.
    let layout = runtime.store_layout();
    let pg_clones: Vec<_> = plan
        .resources
        .iter()
        .filter(|resource| resource.resource == ResourceKind::Postgres)
        .map(|resource| layout.world_clone(&world, resource.resource, &resource.id))
        .collect();
    let epoch = plan_epoch(&plan);
    let stamp_store = WorldStampStore::new(&world_dir);
    let trusted = stamp_store
        .trusted_stamp(&plan, pg_clones.iter().map(|clone| clone.as_path()))
        .is_some();
    // A root world that was `down` and then `up` with an unchanged `Forkfile`, lockfiles, and
    // `PGDATA` carries the same paired checkpoint a fork does, so it reuses the trusted hook path
    // instead of re-running `pg` -> `pre-deploy` -> `api` serially.
    let stamps = stamps_for(project, &stamp_store, trusted);

    let sink_emitter = Arc::clone(&emitter);
    let sink_world = world.as_str().to_owned();
    let sink_pods = pods_dir.display().to_string();
    let sink: Arc<slipfork_world::EventSink> = Arc::new(move |event: BootEvent| {
        if let Ok(mut emitter) = sink_emitter.lock() {
            let mut frame = boot_event_json(&event);
            if matches!(event, BootEvent::Failed { .. }) {
                frame["world"] = json!(sink_world);
                frame["pods_dir"] = json!(sink_pods);
            }
            emitter.send(frame);
        }
    });
    let executor: Arc<slipfork_world::StepExecutor> = {
        let runtime = Arc::clone(&runtime);
        Arc::new(
            move |index: usize, step: &BootStep| -> Result<StepOutcome, BootFailure> {
                runtime.execute(index, step)
            },
        )
    };
    // One composition-root runtime owns every worker; the library only ever receives a `Handle`.
    let tokio_runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(4)
        .enable_all()
        .build()
        .map_err(|error| format!("build the boot runtime: {error}"))?;
    let scheduler = Arc::new(BootScheduler::new(
        tokio_runtime.handle().clone(),
        stamps,
        executor,
        sink,
    ));
    let outcome = scheduler.run(&plan);
    drop(scheduler);
    drop(tokio_runtime);
    match &outcome {
        Ok(()) => {
            // A clean cluster at the end of `up` is what keeps a later fork's crash recovery
            // sub-50ms: one SQL `CHECKPOINT` after post-deploy, then one `syncfs` so the
            // checkpoint is durable in the Btrfs tree the fork will snapshot.
            if let Err(error) = runtime.checkpoint_postgres() {
                eprintln!("slipfork: postgres checkpoint: {error}");
            }
            if let Err(error) = runtime.flush_store() {
                eprintln!("slipfork: store flush: {error}");
            }
            let checkpoint = WorldStamp::checkpoint(&plan, epoch.clone(), runtime.image_stamps());
            if let Err(error) = stamp_store.save(&checkpoint) {
                eprintln!("slipfork: write world stamp: {error}");
            }
            if let Err(error) = write_pg_checkpoints(&pg_clones, &epoch) {
                eprintln!("slipfork: write pg stamp: {error}");
            }
            let url = format!("http://{}.localhost", world.as_str());
            if let Ok(mut emitter) = emitter.lock() {
                emitter.send(json!({
                    "event": "ready",
                    "url": url,
                    "world": world.as_str(),
                    "pods_dir": pods_dir.display().to_string(),
                }));
            }
        }
        Err(error) => {
            // The scheduler already emitted `BootEvent::Failed` for a boot failure; plan-level
            // failures (cycles, poisoned runtime) still need one failure frame.
            if !matches!(error, slipfork_world::Error::Boot { .. })
                && let Ok(mut emitter) = emitter.lock()
            {
                emitter.send(json!({
                    "event": "failed",
                    "label": error.to_string().lines().next().unwrap_or("boot failed"),
                    "line": 0,
                    "message": error.to_string().lines().next().unwrap_or_default(),
                    "world": world.as_str(),
                    "pods_dir": pods_dir.display().to_string(),
                }));
            }
        }
    }
    // `down` reaches the init, which stops every pod and exits; this process follows.
    let awaited = runtime.await_exit_and_cleanup();
    let _ = fs::remove_file(state_path(project));
    awaited
}

/// The stamp store this boot may trust.
///
/// Only a checkpoint whose `world_epoch` still matches the planned DAG **and** whose PG siblings
/// carry the same epoch is trusted; that single trusted value decides `for_fork` for every world,
/// root or fork. A stale epoch (edited `Forkfile`, changed `PGDATA` pair) or a stale input digest
/// (changed lockfile) therefore still re-runs `@<tool>`/`before-start`/`pre-deploy`.
fn stamps_for(project: &Path, store: &WorldStampStore, trusted: bool) -> StampStore {
    if trusted {
        StampStore::for_fork(project, true, store.load())
    } else {
        StampStore::gated(project, false)
    }
}

fn boot_event_json(event: &BootEvent) -> Value {
    match event {
        BootEvent::Plan { rows } => json!({
            "event": "plan",
            "rows": rows
                .iter()
                .map(|row| json!({ "kind": row.kind.as_str(), "name": row.name }))
                .collect::<Vec<_>>(),
            "total": rows.len(),
        }),
        BootEvent::Started {
            step,
            total,
            kind,
            name,
            run_id,
        } => json!({
            "event": "started",
            "step": step + 1,
            "total": total,
            "kind": kind.as_str(),
            "name": name,
            "state": "running",
            "run_id": run_id,
        }),
        BootEvent::Finished {
            kind,
            name,
            elapsed,
            state,
            detail,
            run_id,
            ..
        } => json!({
            "event": "finished",
            "kind": kind.as_str(),
            "name": name,
            "state": state.as_str(),
            "detail": detail,
            "elapsed_ms": elapsed.as_millis(),
            "duration_ms": elapsed.as_millis(),
            "run_id": run_id,
        }),
        BootEvent::UpToDate {
            kind,
            name,
            elapsed,
            ..
        } => json!({
            "event": "up-to-date",
            "kind": kind.as_str(),
            "name": name,
            "state": BootState::UpToDate.as_str(),
            "detail": "up to date",
            "elapsed_ms": elapsed.as_millis(),
            "duration_ms": elapsed.as_millis(),
        }),
        BootEvent::Failed {
            kind,
            name,
            line,
            message,
            remedy,
            elapsed,
            run_id,
            command,
            ..
        } => {
            let mut event = json!({
                "event": "failed",
                "kind": kind.as_str(),
                "name": name,
                "state": BootState::Failed.as_str(),
                "line": line,
                "message": message,
                "elapsed_ms": elapsed.as_millis(),
                "duration_ms": elapsed.as_millis(),
                "run_id": run_id,
                "command": command,
            });
            if let Some(remedy) = remedy {
                event["remedy"] = json!(remedy);
            }
            event
        }
    }
}

fn write_state(project: &Path, world: &WorldName, socket: &Path) -> Result<(), String> {
    let state = WorldState {
        world: world.as_str().to_owned(),
        socket: socket.to_path_buf(),
        project: project.to_path_buf(),
        pid: std::process::id(),
    };
    let path = state_path(project);
    let temporary = path.with_extension("json.tmp");
    let bytes = serde_json::to_vec_pretty(&state).map_err(|error| error.to_string())?;
    fs::write(&temporary, bytes)
        .map_err(|error| format!("write `{}`: {error}", temporary.display()))?;
    fs::rename(&temporary, &path).map_err(|error| format!("rename `{}`: {error}", path.display()))
}

pub(super) struct Emitter {
    open: bool,
}

impl Emitter {
    /// A closed emitter, so unit tests never write NDJSON to a terminal.
    #[cfg(test)]
    pub(super) fn closed() -> Self {
        Self { open: false }
    }

    fn send(&mut self, value: Value) {
        if !self.open {
            return;
        }
        let mut bytes = value.to_string().into_bytes();
        bytes.push(b'\n');
        let mut stdout = std::io::stdout().lock();
        if stdout
            .write_all(&bytes)
            .and_then(|()| stdout.flush())
            .is_err()
        {
            // `up` returned and closed the pipe; the world must keep serving.
            self.open = false;
        }
    }
}

/// Step-scoped NDJSON writer: each concurrent DAG node owns its own line buffer and step identity,
/// locking [`Emitter`] only to flush one complete `log` frame.
pub(super) struct BootSink {
    log: EventLog,
    mirror: std::io::Stderr,
}

impl Clone for BootSink {
    fn clone(&self) -> Self {
        Self {
            log: self.log.clone(),
            mirror: std::io::stderr(),
        }
    }
}

impl BootSink {
    pub(super) fn for_step(
        emitter: Arc<Mutex<Emitter>>,
        step: usize,
        total: usize,
        label: &str,
        name: &str,
    ) -> Self {
        Self {
            log: EventLog::for_step(emitter, step, total, label, name),
            mirror: std::io::stderr(),
        }
    }

    /// Arms a lazy per-step run capture: the id is minted on the first output line, so a `cached`
    /// step that executes nothing never earns one.
    pub(super) fn capture_run(
        mut self,
        logs_root: &Path,
        kind: &str,
        name: &str,
        command: &str,
    ) -> Self {
        self.log.run = Some(Arc::new(Mutex::new(RunTarget::new(
            logs_root, kind, name, command,
        ))));
        self
    }

    /// Appends the catalog line and returns the minted id, or `None` when nothing ran.
    pub(super) fn finish_run(&self, state: &str, duration_ms: u64) -> Option<String> {
        self.log.run.as_ref().and_then(|run| {
            run.lock()
                .ok()
                .and_then(|mut target| target.finish(state, duration_ms))
        })
    }

    /// Drops a no-op capture: no id, no `.log`/`.idx`, no catalog line.
    pub(super) fn discard_run(&self) {
        if let Some(run) = &self.log.run
            && let Ok(mut target) = run.lock()
        {
            target.discard();
        }
    }

    /// Emits one readiness frame pinned to this step.
    pub(super) fn progress(&mut self, message: &str) {
        let _ = writeln!(self.log, "{message}");
        let _ = self.log.flush();
    }
}

impl Write for BootSink {
    fn write(&mut self, buffer: &[u8]) -> std::io::Result<usize> {
        let _ = self.mirror.write_all(buffer);
        self.log.write(buffer)
    }

    fn flush(&mut self) -> std::io::Result<()> {
        let _ = self.mirror.flush();
        self.log.flush()
    }
}

#[derive(Clone)]
struct EventLog {
    emitter: Arc<Mutex<Emitter>>,
    pending: Vec<u8>,
    step: usize,
    total: usize,
    label: String,
    name: String,
    run: Option<Arc<Mutex<RunTarget>>>,
}

/// One step's lazy run capture: begun on the first line, finished with its final state.
struct RunTarget {
    logs_root: PathBuf,
    kind: String,
    name: String,
    command: String,
    capture: Option<RunCapture>,
    closed: bool,
}

impl RunTarget {
    fn new(logs_root: &Path, kind: &str, name: &str, command: &str) -> Self {
        Self {
            logs_root: logs_root.to_path_buf(),
            kind: kind.to_owned(),
            name: name.to_owned(),
            command: command.to_owned(),
            capture: None,
            closed: false,
        }
    }

    fn append(&mut self, text: &str) {
        if self.closed {
            return;
        }
        if self.capture.is_none() {
            match RunCapture::begin(&self.logs_root, &self.kind, &self.name, &self.command) {
                Ok(capture) => self.capture = Some(capture),
                Err(_) => {
                    self.closed = true;
                    return;
                }
            }
        }
        if let Some(capture) = &mut self.capture {
            let _ = capture.append(text);
        }
    }

    fn finish(&mut self, state: &str, duration_ms: u64) -> Option<String> {
        self.closed = true;
        let mut capture = self.capture.take()?;
        let run_id = capture.run_id().to_owned();
        let _ = capture.finish(state, duration_ms);
        Some(run_id)
    }

    fn discard(&mut self) {
        self.closed = true;
        if let Some(capture) = self.capture.take() {
            capture.discard();
        }
    }
}

impl EventLog {
    fn for_step(
        emitter: Arc<Mutex<Emitter>>,
        step: usize,
        total: usize,
        label: &str,
        name: &str,
    ) -> Self {
        Self {
            emitter,
            pending: Vec::new(),
            step,
            total,
            label: label.to_owned(),
            name: name.to_owned(),
            run: None,
        }
    }

    fn forward(&self, line: &[u8]) {
        let text = String::from_utf8_lossy(line);
        let text = strip_ansi(&text);
        let text = text.trim_end_matches('\r');
        if text.trim().is_empty() {
            return;
        }
        if let Some(run) = &self.run
            && let Ok(mut target) = run.lock()
        {
            target.append(text);
        }
        let frame = json!({
            "event": "log",
            "line": text,
            "step": self.step,
            "total": self.total,
            "label": self.label,
            "name": self.name,
        });
        if let Ok(mut emitter) = self.emitter.lock() {
            emitter.send(frame);
        }
    }
}

/// Strips CSI escape sequences from a child's raw line, so a `log` frame never carries ANSI.
pub(super) fn strip_ansi(text: &str) -> std::borrow::Cow<'_, str> {
    if !text.contains('\u{1b}') {
        return std::borrow::Cow::Borrowed(text);
    }
    let mut out = String::with_capacity(text.len());
    let mut chars = text.chars().peekable();
    while let Some(ch) = chars.next() {
        if ch != '\u{1b}' {
            out.push(ch);
            continue;
        }
        match chars.peek() {
            Some('[') => {
                chars.next();
                for ch in chars.by_ref() {
                    if ('@'..='~').contains(&ch) {
                        break;
                    }
                }
            }
            Some(_) => {
                chars.next();
            }
            None => {}
        }
    }
    std::borrow::Cow::Owned(out)
}

impl Write for EventLog {
    fn write(&mut self, buffer: &[u8]) -> std::io::Result<usize> {
        self.pending.extend_from_slice(buffer);
        while let Some(position) = self.pending.iter().position(|byte| *byte == b'\n') {
            let rest = self.pending.split_off(position + 1);
            let line = std::mem::replace(&mut self.pending, rest);
            self.forward(&line[..line.len() - 1]);
        }
        Ok(buffer.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        if !self.pending.is_empty() {
            let line = std::mem::take(&mut self.pending);
            self.forward(&line);
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;
    use std::fs;
    use std::path::{Path, PathBuf};

    use slipfork_world::{
        BootEvent, BootKind, BootStep, HookPhase, PlanRow, StepStamp, WorldPlan, WorldStamp,
        WorldStampStore, plan_epoch, write_pg_checkpoint,
    };

    use super::{boot_event_json, stamps_for};

    /// The conditional-stamp key a `pre-deploy when changed(uv.lock)` hook records.
    const LOCK_STAMP: &str = "pre-deploy-lock";

    fn temp(prefix: &str) -> tempfile::TempDir {
        tempfile::Builder::new()
            .prefix(prefix)
            .tempdir_in("/var/tmp")
            .expect("create a disk temp directory")
    }

    fn hook_plan(command: &str, lock: Option<&str>) -> WorldPlan {
        WorldPlan {
            steps: vec![BootStep {
                kind: BootKind::Hook,
                image: None,
                subject: "api".to_owned(),
                phase: Some(HookPhase::PreDeploy),
                label: "hook api:pre-deploy".to_owned(),
                command: Some(command.to_owned()),
                stamp: lock.map(|digest| StepStamp {
                    key: LOCK_STAMP.to_owned(),
                    digest: digest.to_owned(),
                }),
                uses_env: false,
                line: 7,
                deps: Vec::new(),
            }],
            ..WorldPlan::default()
        }
    }

    /// Writes a real world checkpoint plus the paired PG sibling `slipfork fork` also copies.
    fn checkpoint(root: &Path, plan: &WorldPlan) -> (WorldStampStore, PathBuf) {
        let world_dir = root.join("worlds/demo");
        let pg_clone = root.join("worlds/demo.pg.pg");
        fs::create_dir_all(&world_dir).expect("create the world dir");
        fs::create_dir_all(&pg_clone).expect("create the PG clone");
        let store = WorldStampStore::new(&world_dir);
        let epoch = plan_epoch(plan);
        store
            .save(&WorldStamp::checkpoint(
                plan,
                epoch.clone(),
                BTreeMap::new(),
            ))
            .expect("write the world stamp");
        write_pg_checkpoint(&pg_clone, &epoch).expect("write the PG stamp");
        (store, pg_clone)
    }

    /// A warm root world reuses its paired checkpoint; a changed `Forkfile` or lockfile re-runs it.
    #[test]
    fn a_paired_world_stamp_skips_pre_deploy_and_any_change_reruns_it() {
        let root = temp("slipfork-warm-stamp");
        let project = root.path().join("demo");
        fs::create_dir_all(project.join(".slipfork")).expect("create .slipfork");

        let warm = hook_plan("uv run seed", None);
        let (store, pg_clone) = checkpoint(root.path(), &warm);
        let trusted = |plan: &WorldPlan| store.trusted_stamp(plan, [pg_clone.as_path()]).is_some();
        assert!(
            trusted(&warm),
            "an unchanged plan + paired PG stamp is trusted"
        );
        let stamps = stamps_for(&project, &store, trusted(&warm));
        assert!(
            stamps.is_step_current(&warm.steps, 0),
            "a warm `up` must print `hook api:pre-deploy up to date`"
        );

        // An edited `Forkfile` changes `world_epoch`, so nothing is trusted and the hook runs.
        let edited = hook_plan("uv run seed --fresh", None);
        assert!(
            !trusted(&edited),
            "an edited Forkfile invalidates the checkpoint"
        );
        let stamps = stamps_for(&project, &store, trusted(&edited));
        assert!(
            !stamps.is_step_current(&edited.steps, 0),
            "an edited Forkfile must re-run `pre-deploy`"
        );

        // A lockfile edit keeps the epoch but makes the recorded conditional digest stale.
        let lock = project.join(".slipfork").join(LOCK_STAMP);
        fs::write(&lock, "uv.lock@aaa\n").expect("write the lockfile stamp");
        let pinned = hook_plan("uv run seed", Some("uv.lock@aaa"));
        let (pinned_store, pinned_pg) = checkpoint(root.path(), &pinned);
        let pinned_trusted = |plan: &WorldPlan| {
            pinned_store
                .trusted_stamp(plan, [pinned_pg.as_path()])
                .is_some()
        };
        assert!(pinned_trusted(&pinned));
        let stamps = stamps_for(&project, &pinned_store, pinned_trusted(&pinned));
        assert!(
            stamps.is_step_current(&pinned.steps, 0),
            "an unchanged lockfile stays `up to date`"
        );

        let relocked = hook_plan("uv run seed", Some("uv.lock@bbb"));
        assert_eq!(
            plan_epoch(&relocked),
            plan_epoch(&pinned),
            "same plan shape, same epoch"
        );
        let stamps = stamps_for(&project, &pinned_store, pinned_trusted(&relocked));
        assert!(
            !stamps.is_step_current(&relocked.steps, 0),
            "a changed lockfile must re-run `pre-deploy`"
        );
    }

    /// The `plan` frame is the first NDJSON line, lists every row, and carries zero escapes.
    #[test]
    fn the_plan_frame_comes_first_and_stays_ansi_free() {
        let frames = [
            boot_event_json(&BootEvent::Plan {
                rows: vec![
                    PlanRow {
                        kind: BootKind::Image,
                        name: "base".to_owned(),
                    },
                    PlanRow {
                        kind: BootKind::Service,
                        name: "api".to_owned(),
                    },
                ],
            }),
            boot_event_json(&BootEvent::Started {
                step: 0,
                total: 2,
                kind: BootKind::Image,
                name: "base".to_owned(),
                run_id: None,
            }),
        ];
        let lines: Vec<String> = frames.iter().map(serde_json::Value::to_string).collect();
        assert!(lines[0].starts_with("{\"event\":\"plan\""), "{}", lines[0]);
        assert!(
            lines[0].contains("\"kind\":\"image\",\"name\":\"base\"")
                && lines[0].contains("\"kind\":\"service\",\"name\":\"api\""),
            "{}",
            lines[0]
        );
        assert_eq!(
            serde_json::from_str::<serde_json::Value>(&lines[0]).expect("the plan parses")["total"],
            2
        );
        assert!(lines[1].contains("\"event\":\"started\""));
        for line in &lines {
            assert!(!line.contains('\u{1b}'), "--json stays pure: {line:?}");
        }
    }
}
