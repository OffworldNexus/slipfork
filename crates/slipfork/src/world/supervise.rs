//! Hidden detached-supervisor mode: boot the world, stream typed events, wait for the init.

use std::fs;
use std::io::Write;
use std::path::Path;
use std::process::ExitCode;
use std::sync::{Arc, Mutex};

use serde_json::{Value, json};
use slipfork_world::{
    BootEvent, BootFailure, BootScheduler, BootState, BootStep, LogStore, ResourceKind, StampStore,
    StepOutcome, WorldName, WorldStamp, WorldStampStore, control, is_btrfs_path, plan_epoch,
    world_plan_with, write_pg_checkpoints,
};

use crate::commands::Session;
use crate::world::features::WorldFeatures;
use crate::world::runtime::capture::{RunTarget, strip_ansi};
use crate::world::runtime::frames::{StepPin, failure_facts, log_frame, note_frame, step_frame};

use super::{
    HostEnv, SuperviseArgs, WorldState, fail, runtime::WorldBoot, runtime::WorldRuntime, state_path,
};

impl Session {
    /// Hidden detached-supervisor mode: boot, wait until `down`, then exit.
    pub fn supervise(&self, args: &SuperviseArgs) -> ExitCode {
        // Detach from the terminal's process group so closing the shell cannot kill the world.
        // SAFETY: `setsid(2)` only moves this process into a new session.
        unsafe {
            libc::setsid();
        }
        match supervisor_main(&args.project, &args.world, self.host(), args.no_cache) {
            Ok(()) => ExitCode::SUCCESS,
            Err(message) => fail(message, "inspect `.slipfork/supervisor.log`"),
        }
    }
}

fn supervisor_main(
    project: &Path,
    world: &str,
    env: &HostEnv,
    no_cache: bool,
) -> Result<(), String> {
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
    // One digest per build image: the `Dockerfile` bytes plus the canonical context hash. Folding
    // it into the epoch means a Dockerfile/context edit can never adopt a stale image stamp.
    let image_inputs = crate::world::image_inputs(project, &doc, &plan.images, &features);
    let emitter = Arc::new(Mutex::new(Emitter { open: true }));
    let runtime = Arc::new(WorldRuntime::new(WorldBoot {
        project,
        world: world.clone(),
        doc: &doc,
        plan: &plan,
        features: features.clone(),
        image_inputs: &image_inputs,
        no_cache,
        env,
        emitter: Arc::clone(&emitter),
    })?);
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
    let epoch = plan_epoch(&plan, &features, &image_inputs);
    let stamp_store = WorldStampStore::new(&world_dir);
    let trusted = stamp_store
        .trusted_stamp(
            &plan,
            &features,
            &image_inputs,
            pg_clones.iter().map(|clone| clone.as_path()),
        )
        .is_some();
    // Paired world + PG stamps reuse trusted `@<tool>`/`pre-deploy` digests across warm `up` and `fork`.
    let stamps = stamps_for(project, &stamp_store, trusted);

    let step_commands: Arc<Vec<Option<String>>> =
        Arc::new(plan.steps.iter().map(|step| step.command.clone()).collect());
    let failed_frame: Arc<Mutex<Option<(usize, Value)>>> = Arc::new(Mutex::new(None));
    let sink_emitter = Arc::clone(&emitter);
    let sink_failed = Arc::clone(&failed_frame);
    let sink_commands = Arc::clone(&step_commands);
    let sink: Arc<slipfork_world::EventSink> = Arc::new(move |event: BootEvent| {
        let mut frame = boot_event_json(&event);
        // The plan owns every step's command; the typed event only names the step index.
        if let Some(command) = step_index(&event)
            .and_then(|index| sink_commands.get(index))
            .and_then(Option::as_deref)
        {
            frame["command"] = json!(command);
        }
        if let BootEvent::Failed { step, .. } = event {
            if let Ok(mut slot) = sink_failed.lock()
                && slot.as_ref().is_none_or(|(existing, _)| step < *existing)
            {
                *slot = Some((step, frame));
            }
            return;
        }
        if let Ok(mut emitter) = sink_emitter.lock() {
            emitter.send(frame);
        }
    });
    let typed_exit: Arc<Mutex<Option<(usize, i32)>>> = Arc::new(Mutex::new(None));
    let executor: Arc<slipfork_world::StepExecutor> = {
        let runtime = Arc::clone(&runtime);
        let exit = Arc::clone(&typed_exit);
        Arc::new(
            move |index: usize, step: &BootStep| -> Result<StepOutcome, BootFailure> {
                let outcome = runtime.execute(index, step);
                if let Err(failure) = &outcome
                    && let Some(code) = failure.exit_code
                    && let Ok(mut slot) = exit.lock()
                    && slot.as_ref().is_none_or(|(existing, _)| index < *existing)
                {
                    *slot = Some((index, code));
                }
                outcome
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
            let captured = failed_frame.lock().ok().and_then(|mut slot| slot.take());
            let failed_step = captured.as_ref().map(|(step, _)| *step);
            let mut frame = captured.map(|(_, frame)| frame).unwrap_or_else(|| {
                json!({
                    "event": "failed",
                    "label": error.to_string().lines().next().unwrap_or("boot failed"),
                    "line": 0,
                    "message": error.to_string().lines().next().unwrap_or_default(),
                })
            });
            let record = frame["run_id"].as_str().and_then(|run_id| {
                LogStore::at(runtime.store_layout().store_root(), world.clone())
                    .records()
                    .ok()?
                    .records
                    .into_iter()
                    .find(|record| record.run_id == run_id)
            });
            let plumbing = frame["name"]
                .as_str()
                .and_then(|name| runtime.image_plumbing(name));
            if frame.get("exit_code").is_none()
                && let Some(code) = typed_exit.lock().ok().and_then(|slot| {
                    (*slot).and_then(|(step, code)| {
                        failed_step
                            .is_none_or(|wanted| wanted == step)
                            .then_some(code)
                    })
                })
            {
                frame["exit_code"] = json!(code);
            }
            failure_facts(&mut frame, record.as_ref(), plumbing.as_deref());
            if !runtime.has_live_world() {
                // Zero resources or services entered the world (a cold image `RUN` or pre-pod
                // setup/hook failed): reap `unshare`/`slipfork-world-init`/`slirp4netns` and drop
                // `.slipfork/world.json` before announcing failure so no orphan world survives.
                let stopped = runtime.stop_and_cleanup();
                let _ = fs::remove_file(state_path(project));
                if let Ok(mut emitter) = emitter.lock() {
                    emitter.send(frame);
                }
                return stopped;
            }
            frame["world"] = json!(world.as_str());
            frame["pods_dir"] = json!(pods_dir.display().to_string());
            if let Ok(mut emitter) = emitter.lock() {
                emitter.send(frame);
            }
        }
    }
    // `down` reaches the init, which stops every pod and exits; this process follows.
    let awaited = runtime.await_exit_and_cleanup();
    let _ = fs::remove_file(state_path(project));
    awaited
}

/// The stamp store this boot may trust: only a checkpoint whose `world_epoch` matches the planned
/// DAG **and** whose PG siblings carry the same epoch decides `for_fork`, so a stale epoch or input
/// digest still re-runs `@<tool>`/`before-start`/`pre-deploy`.
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

/// The plan index a boot event names, when it names one.
fn step_index(event: &BootEvent) -> Option<usize> {
    match event {
        BootEvent::Started { step, .. } | BootEvent::Failed { step, .. } => Some(*step),
        _ => None,
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

/// Step-scoped NDJSON writer: one line buffer per DAG node, locking [`Emitter`] only to flush.
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

    /// Opens this step's run capture **before** it executes, so an early error still leaves one.
    pub(super) fn capture_run(
        mut self,
        store: &LogStore,
        project: &Path,
        kind: &str,
        name: &str,
        command: &str,
    ) -> Result<Self, BootFailure> {
        self.log.run = Some(Arc::new(Mutex::new(RunTarget::open(
            store, project, kind, name, command,
        )?)));
        Ok(self)
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

    /// The minted id of this step's run capture, so its record can be annotated while it runs.
    pub(super) fn run_id(&self) -> Option<String> {
        let run = self.log.run.as_ref()?.lock().ok()?;
        run.run_id().map(str::to_owned)
    }

    /// Emits one live build row for `image`'s `index`-th instruction, pinning the child identity.
    pub(super) fn step_event(&mut self, image: &str, event: &slipfork_image::StepEvent<'_>) {
        self.log.image = Some(image.to_owned());
        self.log.index = Some(event.index);
        self.log.send(step_frame(image, event));
    }

    /// Emits one engine narration frame pinned to this step; the renderer shows it only under `-v`.
    pub(super) fn engine_note(&mut self, text: &str) {
        let _ = writeln!(self.mirror, "{text}");
        self.log.note(text);
    }

    /// Emits one readiness note pinned to this step; never a child gutter line or capture line.
    pub(super) fn progress(&mut self, message: &str) {
        self.engine_note(message);
    }

    /// Flushes the supervisor diagnostic mirror.
    pub(super) fn flush(&mut self) {
        let _ = self.mirror.flush();
    }

    /// Appends one stream-tagged child line to the stderr mirror and this step's run capture.
    pub(super) fn append_stream(&mut self, stream: slipfork_world::Stream, line: &str) {
        let text = strip_ansi(line);
        let text = text.trim_end_matches('\r');
        if text.trim().is_empty() {
            return;
        }
        let _ = writeln!(self.mirror, "{text}");
        self.log.append_stream(stream, text);
    }
}

#[derive(Clone)]
struct EventLog {
    emitter: Arc<Mutex<Emitter>>,
    step: usize,
    total: usize,
    label: String,
    name: String,
    image: Option<String>,
    index: Option<usize>,
    run: Option<Arc<Mutex<RunTarget>>>,
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
            step,
            total,
            label: label.to_owned(),
            name: name.to_owned(),
            image: None,
            index: None,
            run: None,
        }
    }

    fn append_stream(&mut self, stream: slipfork_world::Stream, text: &str) {
        let text = strip_ansi(text);
        self.emit(stream, text.trim_end_matches('\r'));
    }

    fn emit(&self, stream: slipfork_world::Stream, text: &str) {
        if text.trim().is_empty() {
            return;
        }
        if let Some(run) = &self.run
            && let Ok(mut target) = run.lock()
        {
            target.append_stream(stream, text);
        }
        let pin = StepPin {
            step: self.step,
            total: self.total,
            label: &self.label,
            name: &self.name,
            image: self.image.as_deref(),
            index: self.index,
        };
        self.send(log_frame(&pin, stream.as_str(), text));
    }

    /// Flushes one engine narration frame pinned to this step, without touching any capture.
    fn note(&self, text: &str) {
        let text = strip_ansi(text);
        let text = text.trim_end_matches('\r');
        if text.is_empty() {
            return;
        }
        let pin = StepPin {
            step: self.step,
            total: self.total,
            label: &self.label,
            name: &self.name,
            image: self.image.as_deref(),
            index: self.index,
        };
        self.send(note_frame(&pin, text));
    }

    /// Flushes one complete frame through the shared emitter, immediately.
    fn send(&self, frame: Value) {
        if let Ok(mut emitter) = self.emitter.lock() {
            emitter.send(frame);
        }
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;
    use std::fs;
    use std::path::{Path, PathBuf};

    use slipfork_forkfile::{FeatureEdit, FeatureSet};
    use slipfork_world::{
        BootEvent, BootKind, BootStep, HookPhase, PlanRow, StepStamp, WorldPlan, WorldStamp,
        WorldStampStore, plan_epoch, write_pg_checkpoint,
    };

    use super::{RunTarget, boot_event_json, stamps_for};

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

    fn fixture_features() -> (FeatureSet, FeatureSet) {
        let checked = slipfork_forkfile::compile("# forkfile: 0.1\n\nfeatures {\n  dev\n}\n")
            .expect("compile minimal feature fixture");
        let dev = checked.document.default_features();
        let prod = checked
            .document
            .resolve_features(false, &[FeatureEdit::Disable("dev".to_owned())])
            .expect("resolve `-F -dev`");
        (dev, prod)
    }

    /// Writes a real world checkpoint plus the paired PG sibling `slipfork fork` also copies.
    fn checkpoint(
        root: &Path,
        plan: &WorldPlan,
        features: &FeatureSet,
    ) -> (WorldStampStore, PathBuf) {
        let world_dir = root.join("worlds/demo");
        let pg_clone = root.join("worlds/demo.pg.pg");
        fs::create_dir_all(&world_dir).expect("create the world dir");
        fs::create_dir_all(&pg_clone).expect("create the PG clone");
        let store = WorldStampStore::new(&world_dir);
        let epoch = plan_epoch(plan, features, &BTreeMap::new());
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

    /// A warm root world reuses its checkpoint; a changed `Forkfile`, features, or lockfile re-runs.
    #[test]
    fn a_paired_world_stamp_skips_pre_deploy_and_any_change_reruns_it() {
        let root = temp("slipfork-warm-stamp");
        let project = root.path().join("demo");
        fs::create_dir_all(project.join(".slipfork")).expect("create .slipfork");
        let (dev, prod) = fixture_features();

        let warm = hook_plan("uv run seed", None);
        let (store, pg_clone) = checkpoint(root.path(), &warm, &dev);
        let trusted = |plan: &WorldPlan, features: &FeatureSet| {
            store
                .trusted_stamp(plan, features, &BTreeMap::new(), [pg_clone.as_path()])
                .is_some()
        };
        assert!(
            trusted(&warm, &dev),
            "an unchanged plan + feature set + paired PG stamp is trusted"
        );
        assert!(
            !trusted(&warm, &prod),
            "disabling `dev` must invalidate the checkpoint even when step commands match"
        );
        let stamps = stamps_for(&project, &store, trusted(&warm, &dev));
        assert!(
            stamps.is_step_current(&warm.steps, 0),
            "a warm `up` must print `hook api:pre-deploy up to date`"
        );
        let prod_stamps = stamps_for(&project, &store, trusted(&warm, &prod));
        assert!(
            !prod_stamps.is_step_current(&warm.steps, 0),
            "a `-F -dev` boot must re-run `pre-deploy` instead of trusting a `dev` stamp"
        );

        // An edited `Forkfile` changes `world_epoch`, so nothing is trusted and the hook runs.
        let edited = hook_plan("uv run seed --fresh", None);
        assert!(
            !trusted(&edited, &dev),
            "an edited Forkfile invalidates the checkpoint"
        );
        let stamps = stamps_for(&project, &store, trusted(&edited, &dev));
        assert!(
            !stamps.is_step_current(&edited.steps, 0),
            "an edited Forkfile must re-run `pre-deploy`"
        );

        // A lockfile edit keeps the epoch but makes the recorded conditional digest stale.
        let lock = project.join(".slipfork").join(LOCK_STAMP);
        fs::write(&lock, "uv.lock@aaa\n").expect("write the lockfile stamp");
        let pinned = hook_plan("uv run seed", Some("uv.lock@aaa"));
        let (pinned_store, pinned_pg) = checkpoint(root.path(), &pinned, &dev);
        let pinned_trusted = |plan: &WorldPlan| {
            pinned_store
                .trusted_stamp(plan, &dev, &BTreeMap::new(), [pinned_pg.as_path()])
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
            plan_epoch(&relocked, &dev, &BTreeMap::new()),
            plan_epoch(&pinned, &dev, &BTreeMap::new()),
            "same plan shape, same epoch"
        );
        let stamps = stamps_for(&project, &pinned_store, pinned_trusted(&relocked));
        assert!(
            !stamps.is_step_current(&relocked.steps, 0),
            "a changed lockfile must re-run `pre-deploy`"
        );
    }

    /// A step that errors before printing still owns an eager capture and a catalog record.
    #[test]
    fn an_eagerly_opened_run_records_a_capture_before_the_step_prints_anything() {
        let root = temp("supervise-run-target");
        let store = slipfork_world::LogStore::at(
            root.path().join("store"),
            slipfork_world::WorldName::parse("demo").expect("valid world name"),
        );
        let project = root.path().join("apps/demo");
        let mut target = RunTarget::open(&store, &project, "setup", "backend", "uv sync")
            .expect("open the capture before the step runs");

        let running = store.records().expect("read the catalog").records;
        assert_eq!(
            running.len(),
            1,
            "begin appends one `running` record: {running:?}"
        );
        let run_id = running[0].run_id.clone();
        assert_eq!(running[0].state, "running");
        assert!(
            store.capture(&run_id).is_file(),
            "the capture exists before the first printed line"
        );

        // The runtime writes this synthetic line precisely so the capture is never empty; the test
        // replays whatever the production formatter produced.
        let exit = crate::world::runtime::exit_line("uv sync", 2);
        target.append_stream(slipfork_world::Stream::Stdout, &exit);
        assert_eq!(target.finish("failed", 7).as_deref(), Some(run_id.as_str()));

        let mut reader = slipfork_world::pod_log::read_range(&store.run_dir(), &run_id, None, None)
            .expect("replay the failed capture");
        let mut lines = Vec::new();
        let _ = reader.read_lines(&mut |line| lines.push(line.text.clone()));
        reader.flush(&mut |line| lines.push(line.text.clone()));
        assert!(
            lines.iter().any(|line| line.contains(&exit)),
            "the failed capture replays the produced text `{exit}`: {lines:?}"
        );

        let finished = store.records().expect("read the catalog").records;
        assert_eq!(finished.len(), 1, "one record per run id: {finished:?}");
        assert_eq!(finished[0].state, "failed");
        assert_eq!(finished[0].duration_ms, 7);
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
