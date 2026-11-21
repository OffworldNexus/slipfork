//! `up`, `down`, and `restart`: spawning the detached supervisor and talking to its world.

use std::fs;
use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;
use std::path::Path;
use std::process::{ExitCode, Stdio};
use std::time::{Duration, Instant};

use serde_json::Value;
use slipfork_world::WorldName;
use slipfork_world::control::{self, InitFrame, PROTOCOL_VERSION, Request};

use crate::commands::Session;
use crate::render::paint;

use super::{DownArgs, RestartArgs, UpArgs, fail, read_state, state_path};

impl Session {
    /// Boots the world in a detached supervisor and streams its phases until ready.
    pub fn up(&self, args: &UpArgs) -> ExitCode {
        let project = self.cwd().to_path_buf();
        let world = match world_name(&project, args.world.as_deref()) {
            Ok(world) => world,
            Err(message) => return fail(message, "pass `--world <name>` using [a-z0-9._-]"),
        };
        if let Some(state) = read_state(&project)
            && SuperviseClient::is_alive(&state.socket)
        {
            return fail(
                format!("world `{}` is already running", state.world),
                "run `slipfork down` first, or use `slipfork restart`",
            );
        }
        let source = match fs::read_to_string(project.join("Forkfile")) {
            Ok(source) => source,
            Err(error) => {
                return fail(
                    format!("cannot read Forkfile: {error}"),
                    "create a `Forkfile` in this directory, then run `slipfork up`",
                );
            }
        };
        if let Err(diagnostics) = slipfork_forkfile::parse(&source) {
            return fail(
                diagnostics
                    .first()
                    .map_or("invalid Forkfile".to_owned(), |item| item.message.clone()),
                diagnostics
                    .first()
                    .map_or("fix the Forkfile", |item| item.next.as_str()),
            );
        }
        let log_dir = project.join(".slipfork");
        if let Err(error) = fs::create_dir_all(&log_dir) {
            return fail(
                format!("cannot create `{}`: {error}", log_dir.display()),
                "check that the project directory is writable",
            );
        }
        let log_path = log_dir.join("supervisor.log");
        let log = match fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&log_path)
        {
            Ok(file) => file,
            Err(error) => {
                return fail(
                    format!("cannot open `{}`: {error}", log_path.display()),
                    "check that `.slipfork/` is writable",
                );
            }
        };
        let exe = self.host().exe.clone();
        let mut command = std::process::Command::new(exe);
        command
            .arg("__supervise")
            .arg("--project")
            .arg(&project)
            .arg("--world")
            .arg(world.as_str());
        if args.json {
            command.arg("--json");
        }
        let mut child = match command
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::from(log))
            .spawn()
        {
            Ok(child) => child,
            Err(error) => {
                return fail(
                    format!("cannot start the world supervisor: {error}"),
                    "check that the slipfork binary is executable",
                );
            }
        };
        let Some(stdout) = child.stdout.take() else {
            return fail(
                "the world supervisor gave no event stream",
                "retry `slipfork up`",
            );
        };
        let mut reader = BufReader::new(stdout);
        if !args.json {
            eprintln!("progress: {}", log_path.display());
        }
        let mut line = String::new();
        loop {
            line.clear();
            match reader.read_line(&mut line) {
                Ok(0) => {
                    return fail(
                        "the world supervisor exited before the world was ready",
                        &format!("read `{}` for the first failure", log_path.display()),
                    );
                }
                Ok(_) => {}
                Err(error) => {
                    return fail(
                        format!("cannot read boot events: {error}"),
                        "retry `slipfork up`",
                    );
                }
            }
            let Ok(event) = serde_json::from_str::<Value>(line.trim()) else {
                continue;
            };
            if args.json {
                print!("{}", line);
                let _ = std::io::stdout().flush();
                match event["event"].as_str() {
                    Some("ready") => return ExitCode::SUCCESS,
                    Some("failed") => return ExitCode::FAILURE,
                    _ => continue,
                }
            }
            match event["event"].as_str().unwrap_or_default() {
                "started" => {
                    let step = event["step"].as_u64().unwrap_or(0);
                    let total = event["total"].as_u64().unwrap_or(0);
                    let line = format!(
                        "{} [{step}/{total}] {} {}",
                        paint("→", "1;36", self.color()),
                        event["kind"].as_str().unwrap_or("step"),
                        event["label"].as_str().unwrap_or("step"),
                    );
                    if self.color() {
                        // Overwrite the in-flight line without emitting a stray escape under
                        // `NO_COLOR`, where a plain newline is the only safe separator.
                        print!("\r{line}");
                        let _ = std::io::stdout().flush();
                    } else {
                        println!("{line}");
                    }
                }
                "log" => {
                    let text = event["line"].as_str().unwrap_or_default();
                    // The event carries its own step: a wave with two setup steps never labels
                    // step 1 output with step 2's index.
                    let prefix = match (event["step"].as_u64(), event["total"].as_u64()) {
                        (Some(step), Some(total)) if total > 0 => format!("[{step}/{total}] "),
                        _ => String::new(),
                    };
                    println!("{}", paint(&format!("{prefix}{text}"), "2", self.color()));
                }
                "finished" | "up-to-date" => {
                    let label = event["label"].as_str().unwrap_or("step");
                    let detail = if event["event"] == "up-to-date" {
                        "up to date".to_owned()
                    } else {
                        let elapsed = event["elapsed_ms"].as_u64().unwrap_or(0);
                        format!(
                            "{}, {}",
                            event["detail"].as_str().unwrap_or_default(),
                            elapsed_text(elapsed)
                        )
                    };
                    let line = format!(
                        "{} {label} {}",
                        paint("✓", "32", self.color()),
                        paint(&format!("({detail})"), "2", self.color())
                    );
                    if self.color() {
                        print!("\r\u{1b}[K{line}\n");
                    } else {
                        println!("{line}");
                    }
                    let _ = std::io::stdout().flush();
                }
                "failed" => {
                    let label = event["label"].as_str().unwrap_or("step");
                    let line = event["line"].as_u64().unwrap_or(0);
                    println!("{} {label}", paint("✗", "31", self.color()));
                    let message = event["message"].as_str().unwrap_or("step failed");
                    eprintln!("error: {message}");
                    match event["remedy"].as_str() {
                        // The step's own remedy wins: a valid Forkfile is not the thing to fix.
                        Some(remedy) => eprintln!("next: {remedy}"),
                        None => eprintln!(
                            "next: fix Forkfile:{line}, then run `slipfork up`; the world stays up for inspection"
                        ),
                    }
                    return ExitCode::FAILURE;
                }
                "ready" => {
                    let url = event["url"].as_str().unwrap_or_default();
                    println!(
                        "{} {}",
                        paint("✓ Ready ->", "1;32", self.color()),
                        paint(url, "1;36", self.color())
                    );
                    println!(
                        "{}",
                        paint(
                            "world keeps running; use `slipfork down` when finished",
                            "2",
                            self.color()
                        )
                    );
                    return ExitCode::SUCCESS;
                }
                _ => {}
            }
        }
    }

    /// Stops the world started from this project through the world init's control plane.
    pub fn down(&self, args: &DownArgs) -> ExitCode {
        let project = self.cwd().to_path_buf();
        let Some(state) = read_state(&project) else {
            return fail(
                "no world is running in this directory",
                "run `slipfork up` to start one",
            );
        };
        // The in-world oracle answers first: a typed `List` says how many pods are about to go.
        let pods = SuperviseClient::pods(&state.socket)
            .ok()
            .map(|pods| pods.len());
        match SuperviseClient::shutdown(&state.socket) {
            Ok(_) => {
                // The supervisor removes `world.json` after its init exits; wait for it so the
                // next `up` never races a half-torn-down world.
                await_exit(state.pid, Duration::from_secs(30));
                let _ = fs::remove_file(state_path(&project));
                if args.json {
                    let mut reply = serde_json::json!({ "stopped": true, "world": state.world });
                    if let Some(count) = pods {
                        reply["pods"] = serde_json::json!(count);
                    }
                    println!("{reply}");
                } else {
                    let detail = match pods {
                        Some(0) => "(none)".to_owned(),
                        Some(count) => format!("{count} pod{}", if count == 1 { "" } else { "s" }),
                        None => "(unknown)".to_owned(),
                    };
                    println!(
                        "{} Stopped {} {detail}",
                        paint("✓", "32", self.color()),
                        state.world
                    );
                }
                ExitCode::SUCCESS
            }
            Err(error) => {
                let _ = fs::remove_file(state_path(&project));
                fail(
                    format!("cannot stop world `{}`: {error}", state.world),
                    "the supervisor is gone; remove `.slipfork/world.json` and run `slipfork up`",
                )
            }
        }
    }

    /// Restarts one service or every service in the running world.
    pub fn restart(&self, args: &RestartArgs) -> ExitCode {
        let project = self.cwd().to_path_buf();
        let Some(state) = read_state(&project) else {
            return fail(
                "no world is running in this directory",
                "run `slipfork up` to start one",
            );
        };
        match SuperviseClient::restart(&state.socket, args.service.as_deref()) {
            Ok(reply) => {
                let count = reply["restarted"].as_u64().unwrap_or(0);
                if args.json {
                    println!(
                        "{}",
                        serde_json::json!({ "restarted": count, "world": state.world })
                    );
                } else {
                    let subject = args.service.as_deref().unwrap_or(&state.world);
                    println!(
                        "{} Restarted {subject} ({count} pod{})",
                        paint("✓", "32", self.color()),
                        if count == 1 { "" } else { "s" }
                    );
                }
                ExitCode::SUCCESS
            }
            Err(error) => fail(
                format!("cannot restart world `{}`: {error}", state.world),
                "run `slipfork up` if the world is no longer running",
            ),
        }
    }
}

/// The one typed client for `<pods>/control.sock`; frames are the canonical `Request` values.
struct SuperviseClient {
    stream: UnixStream,
}

impl SuperviseClient {
    fn is_alive(socket: &Path) -> bool {
        UnixStream::connect(socket).is_ok()
    }

    fn connect(socket: &Path) -> Result<Self, String> {
        let stream = UnixStream::connect(socket)
            .map_err(|error| format!("connect `{}`: {error}", socket.display()))?;
        stream
            .set_read_timeout(Some(Duration::from_secs(120)))
            .map_err(|error| format!("configure `{}`: {error}", socket.display()))?;
        Ok(Self { stream })
    }

    fn call(&mut self, request: &Request) -> Result<Value, String> {
        let bytes = control::encode(request).map_err(|error| error.to_string())?;
        self.stream
            .write_all(&bytes)
            .and_then(|()| self.stream.flush())
            .map_err(|error| format!("send: {error}"))?;
        let mut reader = BufReader::new(&self.stream);
        let mut line = String::new();
        reader
            .read_line(&mut line)
            .map_err(|error| format!("read reply: {error}"))?;
        match control::decode_init_frame(line.trim()).map_err(|error| error.to_string())? {
            InitFrame::Reply { id, ok, error, .. } if id == request.id() => {
                if let Some(error) = error {
                    return Err(if error.message.is_empty() {
                        error.code
                    } else {
                        error.message
                    });
                }
                Ok(ok.unwrap_or(Value::Null))
            }
            InitFrame::Reply { id, .. } => Err(format!("unexpected reply id `{id}`")),
            InitFrame::Stopped { .. } => Err("the world stopped before replying".to_owned()),
            InitFrame::Output { .. } => {
                Err("unexpected output frame on a reply-only channel".to_owned())
            }
            InitFrame::Ready { .. } => Err("the world sent an unexpected ready frame".to_owned()),
        }
    }

    fn shutdown(socket: &Path) -> Result<Value, String> {
        let request = Request::Shutdown {
            v: PROTOCOL_VERSION,
            id: "cli-shutdown".to_owned(),
        };
        Self::connect(socket)?.call(&request)
    }

    /// Lists the pods the world init still tracks, through the one canonical reply parser.
    fn pods(socket: &Path) -> Result<Vec<slipfork_world::PodStatus>, String> {
        let request = Request::List {
            v: PROTOCOL_VERSION,
            id: "cli-list".to_owned(),
        };
        let reply = Self::connect(socket)?.call(&request)?;
        slipfork_world::pod_statuses(&reply).map_err(|error| error.to_string())
    }

    fn restart(socket: &Path, service: Option<&str>) -> Result<Value, String> {
        let request = Request::Restart {
            v: PROTOCOL_VERSION,
            id: "cli-restart".to_owned(),
            service: service.map(str::to_owned),
        };
        Self::connect(socket)?.call(&request)
    }
}

/// Waits until `pid` is gone, bounded so a wedged supervisor never hangs `down` forever.
fn await_exit(pid: u32, timeout: Duration) {
    let Ok(pid) = i32::try_from(pid) else {
        return;
    };
    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline {
        // SAFETY: signal 0 only probes for process existence; nothing is dereferenced.
        if unsafe { libc::kill(pid, 0) } != 0 {
            return;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
}

fn elapsed_text(millis: u64) -> String {
    if millis == 0 {
        "<1ms".to_owned()
    } else if millis < 1000 {
        format!("{millis}ms")
    } else {
        format!("{:.1}s", millis as f64 / 1000.0)
    }
}

fn world_name(project: &Path, requested: Option<&str>) -> Result<WorldName, String> {
    let raw = match requested {
        Some(name) => name.to_owned(),
        None => project
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("world")
            .to_lowercase()
            .chars()
            .map(|ch| {
                if ch.is_ascii_alphanumeric() || ch == '-' || ch == '.' || ch == '_' {
                    ch
                } else {
                    '-'
                }
            })
            .collect(),
    };
    WorldName::parse(&raw).map_err(|error| error.to_string())
}
