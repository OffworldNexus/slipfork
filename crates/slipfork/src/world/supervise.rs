//! Hidden detached-supervisor mode: boot the world, stream typed events, wait for the init.

use std::fs;
use std::io::Write;
use std::path::Path;
use std::process::ExitCode;
use std::sync::{Arc, Mutex};

use serde_json::{Value, json};
use slipfork_world::{
    BootEvent, BootFailure, BootKind, BootStep, StampStore, WorldName, control, is_btrfs_path,
    run_plan, world_plan,
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
        match supervisor_main(&args.project, &args.world, self.host(), args.json) {
            Ok(()) => ExitCode::SUCCESS,
            Err(message) => fail(message, "inspect `.slipfork/supervisor.log`"),
        }
    }
}

fn supervisor_main(project: &Path, world: &str, env: &HostEnv, json: bool) -> Result<(), String> {
    let world = WorldName::parse(world).map_err(|error| error.to_string())?;
    let source = fs::read_to_string(project.join("Forkfile"))
        .map_err(|error| format!("read Forkfile: {error}"))?;
    let doc: slipfork_forkfile::ast::Forkfile =
        slipfork_forkfile::parse(&source).map_err(|diagnostics| {
            diagnostics
                .first()
                .map_or("invalid Forkfile".to_owned(), |item| item.message.clone())
        })?;
    if !is_btrfs_path(project).unwrap_or(false) {
        return Err(format!(
            "`{}` is not on Btrfs\nnext: move the project onto a Btrfs mount",
            project.display()
        ));
    }
    let plan = world_plan(&doc, project);
    let stamps = StampStore::new(project);
    let emitter = Arc::new(Mutex::new(Emitter { open: true }));
    let log: Box<dyn Write + Send> = if json {
        // Under `--json` the typed stream stays machine-readable; builder output goes to the
        // supervisor log, which is this process's stderr.
        Box::new(std::io::stderr())
    } else {
        Box::new(EventLog {
            emitter: Arc::clone(&emitter),
            pending: Vec::new(),
        })
    };
    let runtime = Mutex::new(WorldRuntime::new(project, world.clone(), &plan, env, log)?);
    // The world init binds `<pods>/control.sock`; this process only records its path.
    let socket = {
        let runtime = runtime
            .lock()
            .map_err(|_| "world runtime lock poisoned".to_owned())?;
        control::control_socket_path(runtime.supervisor.pods_dir())
            .map_err(|error| error.to_string())?
    };
    write_state(project, &world, &socket)?;
    let sink_emitter = Arc::clone(&emitter);
    let sink = move |event: BootEvent| {
        if let Ok(mut emitter) = sink_emitter.lock() {
            emitter.send(boot_event_json(&event));
        }
    };
    let execute = |step: &BootStep| match runtime.lock() {
        Ok(mut runtime) => runtime.execute(step),
        Err(_) => Err(BootFailure::new("world runtime lock poisoned")),
    };
    let outcome = run_plan(&plan, &stamps, &execute, &sink);
    let mut runtime = match runtime.into_inner() {
        Ok(runtime) => runtime,
        Err(poisoned) => poisoned.into_inner(),
    };
    match &outcome {
        Ok(()) => {
            let url = format!("http://{}.localhost", world.as_str());
            if let Ok(mut emitter) = emitter.lock() {
                emitter.send(json!({ "event": "ready", "url": url }));
            }
        }
        Err(error) => {
            if let Ok(mut emitter) = emitter.lock() {
                emitter.send(json!({
                    "event": "failed",
                    "label": error.to_string().lines().next().unwrap_or("boot failed"),
                    "line": match error {
                        slipfork_world::Error::Boot { line, .. } => *line,
                        _ => 0,
                    },
                    "message": error.to_string().lines().next().unwrap_or_default(),
                }));
            }
        }
    }
    // `down` reaches the init, which stops every pod and exits; this process follows.
    let awaited = runtime.supervisor.await_exit();
    runtime.cleanup();
    let _ = fs::remove_file(state_path(project));
    awaited.map_err(|error| error.to_string())
}

fn boot_event_json(event: &BootEvent) -> Value {
    match event {
        BootEvent::Started {
            step,
            total,
            kind,
            label,
        } => json!({
            "event": "started",
            "step": step + 1,
            "total": total,
            "kind": kind_name(*kind),
            "label": label,
        }),
        BootEvent::Finished {
            kind,
            label,
            elapsed,
            detail,
            ..
        } => json!({
            "event": "finished",
            "kind": kind_name(*kind),
            "label": label,
            "detail": detail,
            "elapsed_ms": elapsed.as_millis(),
        }),
        BootEvent::UpToDate {
            kind,
            label,
            elapsed,
            ..
        } => json!({
            "event": "up-to-date",
            "kind": kind_name(*kind),
            "label": label,
            "detail": "up to date",
            "elapsed_ms": elapsed.as_millis(),
        }),
        BootEvent::Failed {
            label,
            line,
            message,
            ..
        } => json!({
            "event": "failed",
            "label": label,
            "line": line,
            "message": message,
        }),
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

struct Emitter {
    open: bool,
}

impl Emitter {
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

/// Line-splitting writer that forwards ongoing build output as typed `log` frames.
///
/// Every complete line is flushed as its own NDJSON frame, so a multi-minute `RUN` step never
/// looks stalled; partial writes stay buffered until their newline arrives.
struct EventLog {
    emitter: Arc<Mutex<Emitter>>,
    pending: Vec<u8>,
}

impl EventLog {
    fn forward(&mut self, line: &[u8]) {
        let text = String::from_utf8_lossy(line);
        let text = text.trim_end_matches('\r');
        if text.trim().is_empty() {
            return;
        }
        if let Ok(mut emitter) = self.emitter.lock() {
            emitter.send(json!({ "event": "log", "line": text }));
        }
    }
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
