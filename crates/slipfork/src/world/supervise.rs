//! Hidden detached-supervisor mode: boot the world, stream typed events, wait for the init.

use std::ffi::OsStr;
use std::fs;
use std::io::Write;
use std::path::Path;
use std::process::ExitCode;
use std::sync::{Arc, Mutex};

use serde_json::{Value, json};
use slipfork_world::{
    BootEvent, BootFailure, BootKind, BootScheduler, BootState, BootStep, ResourceKind, StampStore,
    StepOutcome, StoreLayout, WorldName, WorldStamp, WorldStampStore, control, is_btrfs_path,
    plan_epoch, world_plan, write_pg_checkpoints,
};

use crate::commands::Session;

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
    let plan = world_plan(&doc, project).map_err(|error| error.to_string())?;
    let emitter = Arc::new(Mutex::new(Emitter { open: true }));
    let runtime = Arc::new(WorldRuntime::new(
        project,
        world.clone(),
        &doc,
        &plan,
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
    let is_fork = StoreLayout::parse_fork_world(OsStr::new(world.as_str())).is_some();
    let stamps = if is_fork {
        StampStore::for_fork(project, trusted, stamp_store.load())
    } else {
        StampStore::gated(project, trusted)
    };

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

fn boot_event_json(event: &BootEvent) -> Value {
    match event {
        BootEvent::Started {
            step,
            total,
            kind,
            name,
        } => json!({
            "event": "started",
            "step": step + 1,
            "total": total,
            "kind": kind_name(*kind),
            "name": name,
            "state": "running",
        }),
        BootEvent::Finished {
            kind,
            name,
            elapsed,
            state,
            detail,
            ..
        } => json!({
            "event": "finished",
            "kind": kind_name(*kind),
            "name": name,
            "state": state.as_str(),
            "detail": detail,
            "elapsed_ms": elapsed.as_millis(),
            "duration_ms": elapsed.as_millis(),
        }),
        BootEvent::UpToDate {
            kind,
            name,
            elapsed,
            ..
        } => json!({
            "event": "up-to-date",
            "kind": kind_name(*kind),
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
            ..
        } => {
            let mut event = json!({
                "event": "failed",
                "kind": kind_name(*kind),
                "name": name,
                "state": BootState::Failed.as_str(),
                "line": line,
                "message": message,
                "elapsed_ms": elapsed.as_millis(),
                "duration_ms": elapsed.as_millis(),
            });
            if let Some(remedy) = remedy {
                event["remedy"] = json!(remedy);
            }
            event
        }
    }
}

fn kind_name(kind: BootKind) -> &'static str {
    match kind {
        BootKind::Image => "image",
        BootKind::Resource => "resource",
        BootKind::Setup => "setup",
        BootKind::Hook => "hook",
        BootKind::Service => "service",
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
        }
    }

    fn forward(&self, line: &[u8]) {
        let text = String::from_utf8_lossy(line);
        let text = strip_ansi(&text);
        let text = text.trim_end_matches('\r');
        if text.trim().is_empty() {
            return;
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
