//! Journey A: one rich, full-stack Btrfs World E2E journey (`M3.30.4` §2).
//!
//! One daemon, one canonical world (a serving `api`, an `idle` forward target, a real `postgres:18`
//! and a real `redis:8`), one fork, and one idle world: `up -F nope`'s typed refusal, `up` (and its
//! idempotent second `up`), bare `status`, `ps`, `logs`, `exec`, an edited checkout applied by
//! serving component, its whole process group reaped, and the `--json` projection), the fail-closed
//! `db`/`redis` TTY entries, the streaming `forward` bridge, a no-argument `fork` with the client's
//! global/system git config unset (the daemon pins the snapshot author), the duplicate, detached,
//! nested, domain, and project fork refusals, the world-selector round-trip, `open --remote`, `drop`,
//! the unbooted-restart refusal, `prune`, and `down`, plus a world whose pipeline starts nothing
//! (`[no change]`). The journey then proves every booted subvolume and every component process group
//! is reaped.
//!
//! Every helper is real: real disk, the real kernel `KernelBtrfsDriver`, a real `slipforkd`
//! composition root, and the real `slipfork` binary. Off-Btrfs the journey fails closed through
//! [`support::require_btrfs`] (one line naming the missing substrate); it never skips.

use std::fs;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpStream;
use std::path::{Path, PathBuf};
use std::process::{Child, Command};
use std::sync::mpsc;
use std::thread;
use std::time::{Duration, Instant};

use serde_json::Value;

pub mod support;
use support::{
    Daemon, POLL_INTERVAL, SHUTDOWN_DEADLINE, assert_success, disk_tempdir, poll_until,
    repo_with_manifest, run_git, stderr, stdout, wait_for_line,
};

/// The canonical world: a serving `api` (readiness port bound by a distinct child of the `sh`
/// wrapper), an `idle` forward target that accepts then closes, a real `postgres:18`, a real
/// `redis:8`, one ingress, and one `!!` secret so the boot exercises the Open Secret Provider path
/// as well as the process pipeline.
const JOURNEY_SNOW: &str = r#"
images:
  - id: app
    context: .
    dockerfile: Dockerfile

components:
  - id: api
    image: app
    envs: [app]
    internal_ports: [8000]
    run_command: "python3 server.py"

  - id: idle
    image: app
    internal_ports: [8001]
    run_command: 'exec python3 -c "import socket,time; s=socket.socket(); s.bind((\"127.0.0.1\",8001)); s.listen(1); s.accept(); s.close(); time.sleep(300)"'

ingresses:
  - match:
      prefix: "/"
    target:
      component: api
      port: 8000

resources:
  pg_clusters:
    - id: main
      version: "18"
  pg_databases:
    - id: main
      cluster: main
  redis_clusters:
    - id: cache
      version: "8"

modes:
  local:
    envs:
      app:
        BASE_URL: '${{ resources::app.url }}'
        GREETING: v1
        SECRET_KEY: '${{ secrets::SECRET_KEY !! random_string(32) }}'
"#;

/// A world whose single component is disabled in `local`: the pipeline starts nothing, so a restart
/// has no service to bring back (`changed: false`, `restarted: []`).
const IDLE_SNOW: &str = r#"
images:
  - id: app
    context: .
    dockerfile: Dockerfile

components:
  - id: api
    image: app
    run_command: "exec sleep 300"

ingresses:
  - match:
      prefix: "/"
    target:
      component: api
      port: 8000

modes:
  local:
    components:
      api:
        disabled: true
    envs:
      app:
        BASE_URL: '${{ resources::app.url }}'
"#;

/// The serving fixture: the wrapper (`sh -eu -c`) forks the server, so a restart must stop both the
/// wrapper and the process that owns the readiness port.
const SERVER_PY: &str = r#"import http.server
import pathlib

BODY = pathlib.Path("body.txt").read_text(encoding="utf-8").strip()
print("serving " + BODY, flush=True)


class Handler(http.server.BaseHTTPRequestHandler):
    def do_GET(self):
        payload = BODY.encode()
        self.send_response(200)
        self.send_header("Content-Length", str(len(payload)))
        self.end_headers()
        self.wfile.write(payload)

    def log_message(self, fmt, *args):
        pass


http.server.ThreadingHTTPServer(("0.0.0.0", 8000), Handler).serve_forever()
"#;

fn url_count(text: &str) -> usize {
    text.matches("http://").count()
}

/// `/proc/<pid>/stat` field 5 (`pgrp`) of a host process.
fn host_pgrp(pid: u32) -> Option<u32> {
    let stat = fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
    stat.get(stat.rfind(')')? + 1..)?
        .split_whitespace()
        .nth(2)?
        .parse()
        .ok()
}

/// Every host pid whose process group is `pgid`.
fn group_members(pgid: u32) -> Vec<u32> {
    fs::read_dir("/proc")
        .into_iter()
        .flatten()
        .flatten()
        .filter_map(|entry| entry.file_name().to_str()?.parse::<u32>().ok())
        .filter(|pid| host_pgrp(*pid) == Some(pgid))
        .collect()
}

/// The `ps --json` document for the repo's world.
fn ps_json(daemon: &Daemon, cwd: &Path) -> Value {
    let output = daemon.cli(cwd, &["--json", "ps"]);
    assert_success(&output, "ps --json");
    serde_json::from_str(&stdout(&output)).expect("ps json")
}

/// The live process group of one running component (its `nsenter` launcher and descendants).
fn component_group(daemon: &Daemon, cwd: &Path, id: &str) -> (u32, Vec<u32>) {
    let document = ps_json(daemon, cwd);
    let pid = document["services"]
        .as_array()
        .expect("services array")
        .iter()
        .find(|service| service["id"] == id)
        .and_then(|service| service["pid"].as_u64())
        .unwrap_or_else(|| panic!("component '{id}' has no live pid: {document}"));
    let pid = u32::try_from(pid).expect("a pid");
    let pgid = host_pgrp(pid).expect("the component's process group");
    (pgid, group_members(pgid))
}

/// The ids named by the block's single `[restarted] ...` verdict line, or `None` when the restart
/// reported nothing coming back.
fn restarted_ids(block: &str) -> Option<Vec<&str>> {
    let mut verdicts = block
        .lines()
        .filter(|line| line.starts_with("[restarted] "));
    let verdict = verdicts.next()?;
    assert!(
        verdicts.next().is_none(),
        "a restart prints exactly one verdict line: {block}"
    );
    Some(
        verdict
            .trim_start_matches("[restarted] ")
            .split(", ")
            .collect(),
    )
}

/// Every service id the daemon reports `running` in the world's `ps --json` projection, sorted.
fn running_service_ids(daemon: &Daemon, cwd: &Path) -> Vec<String> {
    let document = ps_json(daemon, cwd);
    let mut ids: Vec<String> = document["services"]
        .as_array()
        .expect("services array")
        .iter()
        .filter(|service| service["state"] == "running")
        .filter_map(|service| service["id"].as_str().map(str::to_owned))
        .collect();
    ids.sort_unstable();
    ids
}

/// Every world subvolume directly under `<state_dir>/worlds`.
fn world_subvolumes(state_dir: &Path) -> Vec<PathBuf> {
    let worlds = state_dir.join("worlds");
    let mut entries: Vec<PathBuf> = fs::read_dir(&worlds)
        .map(|directory| directory.flatten().map(|entry| entry.path()).collect())
        .unwrap_or_default();
    entries.sort();
    entries
}

/// The daemon's real ingress port, read from the one `status --json` document.
fn ingress_port(daemon: &Daemon, cwd: &Path) -> u16 {
    let output = daemon.cli(cwd, &["--json", "status"]);
    assert_success(&output, "status --json");
    let document: Value = serde_json::from_str(&stdout(&output)).expect("status json");
    u16::try_from(
        document["ingress_port"]
            .as_u64()
            .unwrap_or_else(|| panic!("ingress_port is always present: {document}")),
    )
    .expect("a u16 port")
}

/// The exact stderr the world verbs carry for `port` (`:80` means silence).
fn ingress_stderr(port: u16) -> String {
    if port == 80 {
        String::new()
    } else {
        format!(
            "! {}\n",
            slipfork::status::ingress_hint(port).expect("a non-:80 ingress is announced")
        )
    }
}

/// The world's `GREETING` from inside the running world (daemon-resolved env, not the host's).
fn world_greeting(daemon: &Daemon, cwd: &Path) -> String {
    let output = daemon.cli(cwd, &["exec", "--", "sh", "-c", "printf %s \"$GREETING\""]);
    assert_success(&output, "exec printf $GREETING");
    stdout(&output).trim().to_owned()
}

/// The retained stdout of the repo's `api` component.
fn service_output(daemon: &Daemon, cwd: &Path) -> String {
    let output = daemon.cli(cwd, &["logs", "api"]);
    assert_success(&output, "logs api");
    stdout(&output)
}

/// A live `slipfork logs -f` child whose stdout is drained by one reader thread, so the journey can
/// assert what a real follower saw without blocking the CLI itself.
struct Follower {
    child: Child,
    lines: mpsc::Receiver<String>,
}

impl Follower {
    /// Attach to the live daemon; the first asserted line proves the attachment is real.
    fn attach(daemon: &Daemon, cwd: &Path) -> Self {
        let mut child = daemon.cli_spawn(cwd, &["logs", "-f"]);
        let stdout = child.stdout.take().expect("the follower's stdout is piped");
        let (sender, lines) = mpsc::channel();
        thread::spawn(move || {
            for line in BufReader::new(stdout).lines() {
                let Ok(line) = line else { break };
                if sender.send(line).is_err() {
                    break;
                }
            }
        });
        Self { child, lines }
    }

    /// The next line matching `predicate`, bounded so a wedged daemon fails instead of hanging.
    fn wait_for(&mut self, what: &str, predicate: impl Fn(&str) -> bool) -> String {
        let deadline = Instant::now() + Duration::from_secs(30);
        loop {
            let remaining = deadline.saturating_duration_since(Instant::now());
            match self.lines.recv_timeout(remaining) {
                Ok(line) if predicate(&line) => return line,
                Ok(_) => {}
                Err(error) => panic!("the follower never saw {what} within 30s: {error}"),
            }
        }
    }

    /// Assert that no already-in-flight line repeats `needle` (the one-divider-per-epoch proof).
    fn assert_no_line_containing(&mut self, needle: &str) {
        while let Ok(line) = self.lines.recv_timeout(Duration::from_millis(300)) {
            assert!(
                !line.contains(needle),
                "a restart epoch emitted more than one `{needle}` line: {line}"
            );
        }
    }

    /// Kill and reap the follower so no piped stdio outlives the journey.
    fn stop(mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// One HTTP/1.0 request through a `slipfork forward` listener, returning the body.
fn http_get(addr: &str) -> String {
    let mut stream = TcpStream::connect(addr).expect("connect to the forward listener");
    stream
        .set_read_timeout(Some(Duration::from_secs(10)))
        .expect("bound the response read");
    stream
        .set_write_timeout(Some(Duration::from_secs(10)))
        .expect("bound the request write");
    write!(
        stream,
        "GET / HTTP/1.0\r\nHost: slipfork\r\nConnection: close\r\n\r\n"
    )
    .expect("write the request");
    let mut response = String::new();
    stream
        .read_to_string(&mut response)
        .unwrap_or_else(|err| panic!("read the response (got {response:?}): {err}"));
    response
        .split("\r\n\r\n")
        .nth(1)
        .expect("a response body")
        .trim()
        .to_owned()
}

#[test]
fn world_lifecycle_journey_boots_edits_forks_and_reaps() {
    let tmp = disk_tempdir("journey-");
    let scratch = tmp.path();
    let repo = repo_with_manifest(scratch, "acme-app", JOURNEY_SNOW);
    fs::write(repo.join("body.txt"), "v1\n").expect("write body");
    fs::write(repo.join("server.py"), SERVER_PY).expect("write server.py");

    let mut daemon = Daemon::start(scratch);
    // Fail closed loudly, never skip: off Btrfs the production `statfs` probe panics with one line
    // naming the path, the found filesystem, and the required one.
    let _driver = support::require_btrfs(&daemon.state_dir);

    // --- up -F nope: one typed refusal, no world, no subvolume -------------
    let refused = daemon.cli(&repo, &["up", "-F", "nope"]);
    assert!(
        !refused.status.success(),
        "an undeclared -F feature must fail closed"
    );
    assert!(
        stdout(&refused).is_empty(),
        "a refused up prints no block: {}",
        stdout(&refused)
    );
    let refusal_text = stderr(&refused);
    let refusal: Vec<&str> = refusal_text
        .lines()
        .filter(|line| !line.is_empty())
        .collect();
    assert_eq!(
        refusal.len(),
        2,
        "one actionable line plus one next: remedy: {refusal:?}"
    );
    assert!(
        refusal[0].contains("feature `nope` is not declared"),
        "the refusal names the undeclared feature: {refusal:?}"
    );
    assert!(
        refusal[1].trim_start().starts_with("next: "),
        "the refusal carries the remedy: {refusal:?}"
    );
    assert_eq!(
        world_subvolumes(&daemon.state_dir),
        Vec::<PathBuf>::new(),
        "a refused up creates no world subvolume"
    );
    let empty: Value = serde_json::from_str(&stdout(&daemon.cli(&repo, &["--json", "status"])))
        .expect("status json");
    assert_eq!(
        empty["worlds"].as_array().map(Vec::len),
        Some(0),
        "a refused up leaves the fleet empty: {empty}"
    );

    // --- up: one world, one origin, the scannable block ---------------------
    let up = daemon.cli(&repo, &["up"]);
    assert_success(&up, "slipfork up");
    assert_eq!(url_count(&stdout(&up)), 1, "up prints exactly one origin");
    let block = stdout(&up);
    assert!(
        block.contains("world     acme-app (running, changed)"),
        "the block names the world, its state, and what changed: {block}"
    );
    assert!(
        block.contains("app       http://acme-app.localhost"),
        "the block carries the one origin, port-less at :80: {block}"
    );
    assert!(
        block.contains("worktree  ") && block.contains("lineage   rootfs "),
        "the block carries the worktree and the single lineage line: {block}"
    );
    assert!(
        !block.contains("[origin]") && !block.contains("[resources]"),
        "the superseded `[origin]` line and happy-path jargon are gone: {block}"
    );
    let port = ingress_port(&daemon, &repo);
    assert_eq!(
        stderr(&up),
        ingress_stderr(port),
        "the block's only stderr is the one loud ingress line"
    );
    assert!(
        !stdout(&up).contains("ingress is :"),
        "the ingress line never reaches stdout"
    );

    // --- bare `status` is the same renderer as `slipfork status` ------------
    let bare = daemon.cli(&repo, &[]);
    assert_success(&bare, "bare slipfork");
    let explicit = daemon.cli(&repo, &["status"]);
    assert_success(&explicit, "slipfork status");
    assert_eq!(
        stdout(&bare),
        stdout(&explicit),
        "bare slipfork must run `slipfork status`, not a second renderer"
    );
    let dashboard = stdout(&bare);
    assert!(
        !dashboard.contains('\x1b'),
        "a piped dashboard carries zero escape bytes: {dashboard:?}"
    );
    let no_color = daemon.cli_with_env(&repo, &["status"], "NO_COLOR", "1");
    assert_success(&no_color, "NO_COLOR status");
    assert_eq!(
        stdout(&no_color),
        dashboard,
        "NO_COLOR changes nothing on a non-terminal stdout"
    );
    let lines: Vec<&str> = dashboard.lines().collect();
    assert!(
        lines[0].starts_with("● acme-app  http://acme-app.localhost"),
        "the canonical row leads its own tree with the one origin: {dashboard}"
    );
    assert!(
        !dashboard.contains("slipforkd")
            && !dashboard.contains("next")
            && !dashboard.contains("lineage"),
        "no daemon header, next spam, or lineage outside -v: {dashboard}"
    );
    let verbose = daemon.cli(&repo, &["-v", "status"]);
    assert_success(&verbose, "status -v");
    assert!(
        stdout(&verbose).contains("lineage"),
        "the -v tree carries the lineage fact: {}",
        stdout(&verbose)
    );

    // --- status: exactly one URL round-trips the printed world id -----------
    let fleet = daemon.cli(&repo, &["status"]);
    assert_success(&fleet, "slipfork status");
    assert_eq!(
        url_count(&stdout(&fleet)),
        1,
        "status prints exactly one URL for one world:\n{}",
        stdout(&fleet)
    );

    // --- ps + logs ---------------------------------------------------------
    let (pgid, members) = component_group(&daemon, &repo, "api");
    assert!(
        !members.is_empty() && members.contains(&host_pgrp_pid(&daemon, &repo)),
        "the component's own `nsenter` launcher is in its process group"
    );
    let logs = daemon.cli(&repo, &["logs", "api"]);
    assert_success(&logs, "logs api");

    // --- exec starts in the world's mapped worktree, quietly ----------------
    let exec = daemon.cli(&repo, &["exec", "--", "sh", "-c", "pwd"]);
    assert_success(&exec, "slipfork exec -- sh -c pwd");
    assert_eq!(
        stdout(&exec).trim(),
        repo.display().to_string(),
        "exec must start in the world's mapped worktree, exactly like the requested cwd"
    );
    assert_eq!(
        stderr(&exec),
        "",
        "world entry must be quiet: no `getcwd` line before the command"
    );

    // --- a non-TTY `secret set` with no piped value refuses with one `next:` remedy
    let secret = daemon.cli(&repo, &["secret", "set", "PIPE_KEY"]);
    assert!(
        !secret.status.success(),
        "an empty non-TTY secret read must refuse"
    );
    let diagnostic = stderr(&secret);
    assert!(
        diagnostic.contains("the secret read from stdin is empty"),
        "the refusal is one line: {diagnostic}"
    );
    assert!(
        diagnostic.contains("next: printf '%s' \"$VALUE\" | slipfork secret set PIPE_KEY"),
        "the remedy is the pipe: {diagnostic}"
    );

    // --- a piped data client prints the world's published connection URI, cleanly
    let db = daemon.cli(&repo, &["db"]);
    assert_success(&db, "a piped `db` prints the connection URI");
    let db_uri = stdout(&db);
    assert!(
        db_uri.starts_with("postgres://slipfork@127.0.0.1:5432/"),
        "the URI is the world's own published resource fact: {db_uri}"
    );
    assert!(
        !db_uri.contains('\x1b'),
        "a piped URI carries no spinner or ANSI bytes: {db_uri:?}"
    );
    let redis = daemon.cli(&repo, &["redis"]);
    assert_success(&redis, "a piped `redis` prints the connection URI");
    let redis_uri = stdout(&redis);
    assert!(
        redis_uri.starts_with("redis://127.0.0.1:6379"),
        "the URI is the world's own published resource fact: {redis_uri}"
    );
    assert!(
        !redis_uri.contains('\x1b'),
        "a piped URI carries no spinner or ANSI bytes: {redis_uri:?}"
    );

    // --- a real TTY makes the data client interactive, never a URI printer ---------
    let interactive = daemon.cli_with_pty(&repo, &["redis"], "PING\nQUIT\n");
    assert_success(&interactive, "a TTY `redis` opens the interactive client");
    let pty_text = stdout(&interactive);
    assert!(
        pty_text.contains("PONG"),
        "the interactive redis-cli answered over the pseudo-terminal: {pty_text:?}"
    );
    assert!(
        !pty_text.contains("redis://"),
        "an interactive TTY entry never prints the piped URI leg: {pty_text:?}"
    );

    // --- second `up` is idempotent and never mints a second URL -------------
    let up_again = daemon.cli(&repo, &["up"]);
    assert_success(&up_again, "second slipfork up");
    assert_eq!(
        url_count(&stdout(&up_again)),
        1,
        "a second up never mints a second origin: {}",
        stdout(&up_again)
    );

    // --- open: the remote recipe, and the fail-closed no-opener path --------
    let remote = daemon.cli(&repo, &["open", "--remote"]);
    assert_success(&remote, "slipfork open --remote");
    assert!(
        stdout(&remote)
            .lines()
            .any(|line| line.contains("ssh") || line.contains("slipfork")),
        "`--remote` prints the recipe instead of opening a browser: {}",
        stdout(&remote)
    );
    // The `--json` recipe carries the paste-ready tunnel and omits the default `:80`.
    let remote_json = daemon.cli(&repo, &["--json", "open", "--remote"]);
    assert_success(&remote_json, "open --remote --json");
    let document: Value = serde_json::from_str(&stdout(&remote_json)).expect("open json");
    let ssh_line = document["remote"]["ssh"]
        .as_str()
        .expect("the --json document carries `remote.ssh`");
    assert!(
        ssh_line.starts_with("ssh -N -L 8080:127.0.0.1:"),
        "the printed line is the paste-ready tunnel: {ssh_line}"
    );
    assert_eq!(
        document["remote"]["url"], "http://acme-app.localhost:8080",
        "the remote URL is the world's verified authority on the fixed laptop port: {document}"
    );
    // The one genuine port-80 case renders port-less.
    let local_80 = daemon.cli(&repo, &["--json", "open", "--remote", "-p", "80"]);
    assert_success(&local_80, "open --remote -p 80 --json");
    let document: Value = serde_json::from_str(&stdout(&local_80)).expect("open -p 80 json");
    assert_eq!(
        document["remote"]["url"], "http://acme-app.localhost",
        "a port-80 forward omits the redundant `:80`: {document}"
    );
    // A headless host reached over SSH auto-selects the recipe without `-r`.
    let headless = daemon.cli_with_env(
        &repo,
        &["open"],
        "SSH_CONNECTION",
        "203.0.113.9 41000 10.0.0.7 22",
    );
    assert_success(&headless, "an SSH session auto-selects `open --remote`");
    let headless_text = stdout(&headless);
    assert!(
        headless_text.contains("ssh -N -L 8080:127.0.0.1:"),
        "the auto-selected recipe prints the tunnel: {headless_text}"
    );
    assert!(
        headless_text.contains("@10.0.0.7"),
        "the recipe targets $SSH_CONNECTION's server address: {headless_text}"
    );
    let restricted = support::tool_path(scratch, "restricted-path", &["git", "sh", "uname"]);
    let refused = daemon.cli_with_path(&repo, &restricted, &["open"]);
    assert!(
        !refused.status.success(),
        "no opener must fail `slipfork open` closed"
    );
    assert!(
        stderr(&refused).contains("slipfork open --remote"),
        "one actionable line naming the remote recipe: {}",
        stderr(&refused)
    );
    assert!(
        stdout(&refused).trim().is_empty(),
        "a refused open never prints a confirmation: {}",
        stdout(&refused)
    );

    // --- the forward bridge streams a real request and isolates a dead target
    let mut forward = daemon.cli_spawn(
        &repo,
        &["--json", "forward", "components.api", "components.idle"],
    );
    let line = wait_for_line(&mut forward, |line| line.contains("host_addr"));
    let summary: Value = serde_json::from_str(&line).expect("forward json");
    let targets = summary["targets"].as_array().expect("targets");
    let api_addr = targets
        .iter()
        .find(|target| target["id"] == "components.api")
        .and_then(|target| target["host_addr"].as_str())
        .expect("components.api host addr")
        .to_owned();
    let dead_addr = targets
        .iter()
        .find(|target| target["id"] == "components.idle")
        .and_then(|target| target["host_addr"].as_str())
        .expect("components.idle host addr")
        .to_owned();
    assert_eq!(
        http_get(&api_addr),
        "v1",
        "the daemon-owned bridge must reach the world listener"
    );
    let mut dead_stream = TcpStream::connect(&dead_addr).expect("connect to the dead target");
    dead_stream
        .set_read_timeout(Some(Duration::from_secs(10)))
        .expect("dead read timeout");
    let mut sink = Vec::new();
    let _ = dead_stream.read_to_end(&mut sink);
    assert_eq!(
        http_get(&api_addr),
        "v1",
        "a dead forward target closes that one connection and nothing else"
    );
    let _ = forward.kill();
    let _ = forward.wait();
    poll_until(SHUTDOWN_DEADLINE, POLL_INTERVAL, || {
        if TcpStream::connect(&api_addr).is_err() {
            Ok(())
        } else {
            Err(format!("{api_addr} still accepts connections"))
        }
    })
    .unwrap_or_else(|last| {
        panic!(
            "the host listener must close within {SHUTDOWN_DEADLINE:?} when the CLI disconnects: {last}"
        )
    });

    // --- a live follower sees exactly one divider across `restart` ----------
    let mut follower = Follower::attach(&daemon, &repo);
    follower.wait_for("the pre-restart retained output", |line| {
        line.contains("api") && line.contains("serving v1")
    });

    // --- edited checkout: `restart` reaps the whole sh-wrapped component ----
    fs::write(repo.join("body.txt"), "v2\n").expect("edit the served body");
    let restart = daemon.cli(&repo, &["restart"]);
    assert_success(&restart, "restart after a body edit");
    let restart_block = stdout(&restart);
    let restarted = restarted_ids(&restart_block)
        .unwrap_or_else(|| panic!("a changed restart names what came back: {restart_block}"));
    for id in ["api", "pg_clusters.main", "redis_clusters.cache"] {
        assert!(
            restarted.contains(&id),
            "restart names `{id}` beside the edited component: {restart_block}"
        );
    }
    let mut brought_back: Vec<String> = restarted.iter().map(|id| (*id).to_owned()).collect();
    brought_back.sort_unstable();
    assert_eq!(
        brought_back,
        running_service_ids(&daemon, &repo),
        "the verdict names exactly the services the daemon reports running: {restart_block}"
    );
    assert!(
        !restart_block.contains("[no change]"),
        "an edited restart never claims nothing changed: {restart_block}"
    );
    assert_eq!(
        url_count(&stdout(&restart)),
        1,
        "restart keeps exactly one origin: {}",
        stdout(&restart)
    );
    assert_eq!(
        stderr(&restart),
        ingress_stderr(port),
        "exactly one ingress line on stderr, never on stdout"
    );
    assert!(
        service_output(&daemon, &repo).contains("serving v2"),
        "the reloaded component is the new process: {}",
        service_output(&daemon, &repo)
    );
    let stderr_logs = daemon.cli(&repo, &["logs", "--stderr", "api"]);
    assert_success(&stderr_logs, "logs --stderr api");
    let stderr_text = stdout(&stderr_logs);
    assert!(
        !stderr_text.contains("Address already in use")
            && !stderr_text.contains("EADDRINUSE")
            && !stderr_text.contains("Errno 98"),
        "the new component must bind its port: {stderr_text}"
    );
    let reaped = poll_until(SHUTDOWN_DEADLINE, POLL_INTERVAL, || {
        if let Some(pid) = members
            .iter()
            .find(|pid| Path::new(&format!("/proc/{pid}")).exists())
        {
            Err(format!(
                "old component pid {pid} is still alive in group {pgid}"
            ))
        } else {
            Ok(())
        }
    });
    assert!(
        reaped.is_ok(),
        "restart must stop the whole component (wrapper and its child): {reaped:?}"
    );
    assert!(
        !stdout(&daemon.cli(&repo, &["logs", "api"])).contains("serving v1"),
        "the restart must not serve the old body"
    );
    let divider = follower.wait_for("a restart divider", |line| line.contains("── restarted "));
    assert!(
        divider.starts_with("── restarted "),
        "the divider is its own line, never glued to component output: {divider:?}"
    );
    follower.wait_for("the post-restart retained output", |line| {
        line.contains("api") && line.contains("serving v2")
    });
    follower.assert_no_line_containing("── restarted ");
    follower.stop();

    // --- an edited `snow.yml` is applied by `restart`, never by `up` --------
    let snow = fs::read_to_string(repo.join("snow.yml")).expect("read snow.yml");
    fs::write(
        repo.join("snow.yml"),
        snow.replace("GREETING: v1", "GREETING: v2"),
    )
    .expect("edit snow.yml");
    assert_eq!(
        world_greeting(&daemon, &repo),
        "v1",
        "the boot resolved the env"
    );
    let unchanged = daemon.cli(&repo, &["up"]);
    assert_success(&unchanged, "second up after the edit");
    assert!(
        stdout(&unchanged).contains(
            "world     acme-app (running, unchanged — run `slipfork restart` to apply edits)"
        ),
        "an unchanged up names the verb that applies edits: {}",
        stdout(&unchanged)
    );
    assert_eq!(
        world_greeting(&daemon, &repo),
        "v1",
        "up alone never applies an edited snow.yml"
    );
    let applied = daemon.cli(&repo, &["restart"]);
    assert_success(&applied, "restart after an edited snow.yml");
    let block = stdout(&applied);
    assert!(
        !block.contains('\x1b'),
        "the piped restart block carries zero escape bytes: {block:?}"
    );
    for component in ["api", "idle"] {
        assert!(
            block.lines().any(|line| {
                line.starts_with("[ready]") && line.split_whitespace().nth(1) == Some(component)
            }),
            "restart announces `{component}` ready: {block}"
        );
    }
    let restarted = restarted_ids(&block)
        .unwrap_or_else(|| panic!("a changed restart names what came back: {block}"));
    for id in ["api", "pg_clusters.main", "redis_clusters.cache"] {
        assert!(
            restarted.contains(&id),
            "the verdict names `{id}` beside the edited component: {block}"
        );
    }
    let mut brought_back: Vec<String> = restarted.iter().map(|id| (*id).to_owned()).collect();
    brought_back.sort_unstable();
    assert_eq!(
        brought_back,
        running_service_ids(&daemon, &repo),
        "the verdict names exactly the services the daemon reports running: {block}"
    );
    assert!(
        !block.contains("[no change]"),
        "a changed restart never claims nothing changed: {block}"
    );
    assert!(
        block.contains("world     acme-app (running, changed)"),
        "the changed world row: {block}"
    );
    assert!(
        block.contains("app       http://acme-app.localhost"),
        "the app row carries the world origin whatever its ephemeral port: {block}"
    );
    for row in [
        "worktree  ",
        "next      slipfork open | ",
        "lineage   rootfs ",
    ] {
        assert!(
            block.lines().any(|line| line.starts_with(row)),
            "the block carries a `{row}` row: {block}"
        );
    }
    assert_eq!(
        world_greeting(&daemon, &repo),
        "v2",
        "restart applied the edit"
    );

    // `--json` is the daemon's own summary (one projection), with no human text and no stderr.
    let document = daemon.cli(&repo, &["--json", "restart"]);
    assert_success(&document, "restart --json");
    assert_eq!(
        stderr(&document),
        "",
        "--json never prints the ingress line"
    );
    let document: Value = serde_json::from_str(&stdout(&document)).expect("restart json");
    let mut keys: Vec<&str> = document
        .as_object()
        .unwrap_or_else(|| panic!("an object document: {document}"))
        .keys()
        .map(String::as_str)
        .collect();
    keys.sort_unstable();
    assert_eq!(
        keys,
        vec![
            "changed",
            "lineage",
            "next",
            "ok",
            "origin",
            "resources",
            "restarted",
            "services",
            "state",
            "worktree",
            "world",
        ],
        "every documented key travels: {document}"
    );
    assert_eq!(document["ok"], true);
    assert_eq!(document["world"], "acme-app");
    assert_eq!(document["state"], "running");
    assert_eq!(document["changed"], true);
    let mut json_restarted: Vec<String> = document["restarted"]
        .as_array()
        .unwrap_or_else(|| panic!("the restarted ids travel as an array: {document}"))
        .iter()
        .map(|id| id.as_str().expect("a restarted id").to_owned())
        .collect();
    json_restarted.sort_unstable();
    assert_eq!(
        json_restarted,
        running_service_ids(&daemon, &repo),
        "the json projection names the same restarted set the daemon observed: {document}"
    );
    for key in ["rootfs", "services", "seed", "base"] {
        assert!(
            document["lineage"].get(key).is_some(),
            "lineage.{key} travels as-is: {document}"
        );
    }

    // --- no-argument `fork` derives the branch with the client git config unset
    run_git(&repo, &["switch", "-q", "-c", "feat/ux-pass"]);
    let fork = Command::new(env!("CARGO_BIN_EXE_slipfork"))
        .arg("--state-dir")
        .arg(&daemon.state_dir)
        .arg("fork")
        .current_dir(&repo)
        .env(
            "XDG_CACHE_HOME",
            daemon.state_dir.parent().expect("state parent"),
        )
        .env("XDG_DATA_HOME", daemon.state_dir.join("xdg-data"))
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_SYSTEM", "/dev/null")
        .output()
        .expect("spawn slipfork fork");
    assert_success(&fork, "slipfork fork (branch-derived)");
    let worktree = repo.join(".worktrees/feat-ux-pass");
    let fork_block = stdout(&fork);
    assert!(
        fork_block.contains("world     acme-app:feat-ux-pass (running, changed)"),
        "`feat/ux-pass` becomes the fork `feat-ux-pass`: {fork_block}"
    );
    assert!(
        fork_block.contains("app       http://feat-ux-pass.acme-app.localhost"),
        "the origin is the fork URL, port-less at :80: {fork_block}"
    );
    assert!(
        fork_block.contains(&format!("worktree  {}\n", worktree.display())),
        "the block names the absolute `.worktrees/<fork>` checkout: {fork_block}"
    );
    assert!(
        fork_block.contains(&format!("cd        cd {}\n", worktree.display())),
        "the copy-pasteable `cd` line uses that same path: {fork_block}"
    );
    assert_eq!(
        url_count(&fork_block),
        1,
        "exactly one origin URL: {fork_block}"
    );
    assert!(
        !fork_block.contains("<FORK>") && !fork_block.contains("[origin]"),
        "no placeholder or superseded line survives: {fork_block}"
    );
    assert!(
        worktree.join("snow.yml").is_file(),
        "the derived fork worktree is a real checkout"
    );

    // --- the printed ids round-trip through `--world` ----------------------
    let json_status = daemon.cli(&repo, &["--json", "status"]);
    assert_success(&json_status, "status --json");
    let document: Value = serde_json::from_str(&stdout(&json_status)).expect("status json");
    let worlds = document["worlds"].as_array().expect("worlds is an array");
    assert_eq!(worlds.len(), 2, "canonical plus one fork");
    for world in worlds {
        let origin = world["origin"].as_str().expect("origin string");
        assert_eq!(
            origin.matches("http://").count(),
            1,
            "every world must carry exactly one origin URL: {origin}"
        );
        assert!(world["worktree"].as_str().is_some(), "worktree path");
    }
    let canonical_id = worlds
        .iter()
        .find(|world| world["worktree"].as_str() == Some(repo.to_string_lossy().as_ref()))
        .and_then(|world| world["world"].as_str())
        .expect("canonical world row")
        .to_owned();
    let fork_id = worlds
        .iter()
        .find(|world| world["worktree"].as_str() == Some(worktree.to_string_lossy().as_ref()))
        .and_then(|world| world["world"].as_str())
        .expect("fork world row")
        .to_owned();
    assert_eq!(canonical_id, "acme-app");
    assert_eq!(fork_id, "acme-app:feat-ux-pass");
    let exec_world = |selector: &str| -> Value {
        let output = daemon.cli(
            &repo,
            &["--json", "exec", "--world", selector, "--", "true"],
        );
        assert_success(&output, &format!("exec --world {selector}"));
        serde_json::from_str(&stdout(&output)).expect("exec json")
    };
    assert_eq!(exec_world(&canonical_id)["world"], canonical_id);
    assert_eq!(exec_world(&fork_id)["world"], fork_id);
    assert_eq!(exec_world("feat-ux-pass")["world"], fork_id);
    let foreign = daemon.cli(
        &repo,
        &["--json", "exec", "--world", "other:feat-x", "--", "true"],
    );
    assert!(!foreign.status.success(), "a foreign project must fail");
    let foreign = stdout(&foreign);
    assert!(
        foreign.contains("belongs to project 'other'") && foreign.contains("acme-app"),
        "the foreign-project refusal must name both projects: {foreign}"
    );
    let multi = daemon.cli(
        &repo,
        &["--json", "exec", "--world", "acme-app:feat:x", "--", "true"],
    );
    assert!(!multi.status.success(), "two separators must fail");
    assert!(
        stdout(&multi).contains("at most one ':'"),
        "the multi-separator refusal must name the grammar: {}",
        stdout(&multi)
    );

    // --- duplicate-fork refusal names the boot command, one line -----------
    let again = daemon.cli(&repo, &["fork"]);
    assert!(
        !again.status.success(),
        "forking the same branch twice must fail closed"
    );
    let diagnostic = stderr(&again);
    assert_eq!(
        diagnostic.lines().count(),
        1,
        "one line, never a git advice block: {diagnostic}"
    );
    assert!(
        diagnostic.contains("fork 'feat-ux-pass' already exists")
            && diagnostic.contains("`slipfork up --world acme-app:feat-ux-pass`"),
        "the refusal names the boot command for the existing world: {diagnostic}"
    );

    // --- a detached HEAD and a nameless fork from inside a fork refuse -----
    run_git(&repo, &["checkout", "-q", "--detach"]);
    let detached = daemon.cli(&repo, &["fork"]);
    assert!(
        !detached.status.success(),
        "a detached HEAD must fail closed"
    );
    let diagnostic = stderr(&detached);
    assert_eq!(
        diagnostic.lines().count(),
        1,
        "one line, never a git advice block: {diagnostic}"
    );
    assert!(
        diagnostic.contains("checkout has no active branch")
            && diagnostic.contains("`slipfork fork <name>`"),
        "the refusal names the explicit-name remedy: {diagnostic}"
    );
    run_git(&repo, &["switch", "-q", "-c", "feat/inner"]);
    assert_success(&daemon.cli(&repo, &["fork"]), "fork feat/inner");
    let nested = daemon.cli(&repo.join(".worktrees/feat-inner"), &["fork"]);
    assert!(
        !nested.status.success(),
        "a nameless fork from a fork worktree must fail closed"
    );
    let diagnostic = stderr(&nested);
    assert_eq!(
        diagnostic.lines().count(),
        1,
        "one line, never a git advice block: {diagnostic}"
    );
    assert!(
        diagnostic.contains("already inside fork worktree '.worktrees/feat-inner'")
            && diagnostic.contains(&format!("repository root {}", repo.display())),
        "the refusal names the worktree and the repository root: {diagnostic}"
    );
    assert_success(
        &daemon.cli(&repo, &["drop", "feat-inner"]),
        "drop feat-inner",
    );

    // --- a declared domain and the project's own name are not fork names ---
    let domains_repo = repo_with_manifest(
        scratch,
        "acme-domains",
        &format!("domains:\n  - id: admin\n{JOURNEY_SNOW}"),
    );
    run_git(&domains_repo, &["switch", "-q", "-c", "admin"]);
    let domain = daemon.cli(&domains_repo, &["fork"]);
    assert!(
        !domain.status.success(),
        "a declared `domains[].id` must never be forked over"
    );
    let diagnostic = stderr(&domain);
    assert_eq!(
        diagnostic.lines().count(),
        1,
        "one line, never a git advice block: {diagnostic}"
    );
    assert!(
        diagnostic.contains("collides with snow.yml domains[].id 'admin'")
            && diagnostic.contains("`slipfork fork <name>`"),
        "the refusal names the real domain collision and the explicit-name remedy: {diagnostic}"
    );
    run_git(&domains_repo, &["switch", "-q", "-c", "acme-domains"]);
    let project = daemon.cli(&domains_repo, &["fork"]);
    assert!(
        !project.status.success(),
        "the project's own name is not a fork name"
    );
    let diagnostic = stderr(&project);
    assert_eq!(
        diagnostic.lines().count(),
        1,
        "one line, never a git advice block: {diagnostic}"
    );
    assert!(
        diagnostic.contains("branch 'acme-domains' collides with project 'acme-domains'")
            && diagnostic.contains("`slipfork fork <name>`"),
        "the refusal is the project-collision line: {diagnostic}"
    );

    // --- unbooted-restart refusal: never silently booted --------------------
    let before = world_subvolumes(&daemon.state_dir);
    let unbooted = daemon.cli(&repo, &["restart", "--world", "acme-app:never"]);
    assert!(
        !unbooted.status.success(),
        "restarting a world the daemon never booted must fail closed"
    );
    let diagnostic = stderr(&unbooted);
    assert!(
        diagnostic.contains("is not running") && diagnostic.contains("`slipfork up`"),
        "the refusal names `slipfork up`: {diagnostic}"
    );
    assert_eq!(
        world_subvolumes(&daemon.state_dir),
        before,
        "a refused restart must never create a world subvolume"
    );

    // --- a world whose pipeline starts nothing reports `[no change]` --------
    let idle_repo = repo_with_manifest(scratch, "acme-idle", IDLE_SNOW);
    assert_success(&daemon.cli(&idle_repo, &["up"]), "up the idle world");
    let idle_restart = daemon.cli(&idle_repo, &["restart"]);
    assert_success(&idle_restart, "restart an idle world");
    let idle_block = stdout(&idle_restart);
    assert!(
        idle_block.lines().any(|line| line == "[no change]"),
        "nothing came back, so the verdict says so: {idle_block}"
    );
    assert!(
        idle_block.contains("world     acme-idle (running, unchanged)"),
        "the unchanged world row: {idle_block}"
    );
    assert!(!idle_block.contains("[restarted]"), "{idle_block}");
    assert!(
        !idle_block.contains("[ready]"),
        "a disabled pipeline announces nothing ready: {idle_block}"
    );
    let idle_json = daemon.cli(&idle_repo, &["--json", "restart"]);
    assert_success(&idle_json, "restart --json");
    let idle_doc: Value = serde_json::from_str(&stdout(&idle_json)).expect("restart json");
    assert_eq!(idle_doc["changed"], false, "{idle_doc}");
    assert_eq!(
        idle_doc["restarted"].as_array().map(Vec::len),
        Some(0),
        "an empty id list, never a guess: {idle_doc}"
    );
    assert_success(&daemon.cli(&idle_repo, &["down"]), "down the idle world");

    // --- a dirty throwaway fork is refused until `-f` ----------------------
    let probe_fork = daemon.cli(&repo, &["fork", "dirty-probe"]);
    assert_success(&probe_fork, "fork the throwaway `dirty-probe`");
    let probe_worktree = repo.join(".worktrees/dirty-probe");
    assert!(
        probe_worktree.is_dir(),
        "the throwaway fork has a real checkout: {}",
        probe_worktree.display()
    );
    fs::write(probe_worktree.join("dirty.txt"), "dirty\n").expect("dirty marker");
    let refused = daemon.cli(&probe_worktree, &["drop"]);
    assert!(
        !refused.status.success(),
        "a dirty fork must refuse to drop: stdout={:?}",
        stdout(&refused)
    );
    let diagnostic = stderr(&refused);
    assert!(
        diagnostic.contains("has 1 uncommitted path(s) in its checkout"),
        "the refusal counts the dirty paths: {diagnostic}"
    );
    assert!(
        diagnostic.contains("slipfork drop dirty-probe --force"),
        "the remedy names `--force`: {diagnostic}"
    );
    assert!(
        probe_worktree.exists(),
        "a refused drop keeps the checkout: {}",
        probe_worktree.display()
    );
    let forced = daemon.cli(&probe_worktree, &["drop", "--force"]);
    assert_success(
        &forced,
        "drop --force discards the dirty throwaway checkout",
    );
    assert!(
        !probe_worktree.exists(),
        "`-f` removes the dirty throwaway checkout"
    );
    assert!(
        worktree.exists(),
        "the retained fork is untouched by the throwaway probe: {}",
        worktree.display()
    );

    // --- `drop` releases the fork by its printed id, `down` the canonical world
    let drop = daemon.cli(&repo, &["drop", fork_id.as_str()]);
    assert_success(&drop, "drop the printed fork id");
    assert!(
        !worktree.exists(),
        "drop removes the fork's real worktree: {}",
        worktree.display()
    );
    let after = daemon.cli(&repo, &["--json", "status"]);
    assert_success(&after, "status after drop");
    let document: Value = serde_json::from_str(&stdout(&after)).expect("json");
    let remaining = document["worlds"].as_array().expect("worlds array");
    assert_eq!(remaining.len(), 1);
    assert_eq!(
        remaining[0]["origin"]
            .as_str()
            .expect("origin")
            .matches("http://")
            .count(),
        1
    );

    let prune = daemon.cli(&repo, &["prune", "--dry-run"]);
    assert_success(&prune, "slipfork prune --dry-run");

    // --- a follower survives `down` + `up`: one divider, stream continues --
    let mut follower = Follower::attach(&daemon, &repo);
    follower.wait_for("the canonical world's retained api output", |line| {
        line.contains("api") && line.contains("serving v2")
    });
    let cycle_down = daemon.cli(&repo, &["down"]);
    assert_success(&cycle_down, "down under a live follower");
    let cycle_up = daemon.cli(&repo, &["up"]);
    assert_success(&cycle_up, "up under a live follower");
    let divider = follower.wait_for("the down + up divider", |line| {
        line.contains("── restarted ")
    });
    assert!(
        divider.starts_with("── restarted "),
        "down + up emits one divider line to the attached follower: {divider:?}"
    );
    follower.wait_for("the re-booted api output", |line| {
        line.contains("api") && line.contains("serving v2")
    });
    follower.assert_no_line_containing("── restarted ");
    follower.stop();

    let down = daemon.cli(&repo, &["down"]);
    assert_success(&down, "slipfork down");
    assert!(
        stdout(&down).contains("[down]"),
        "expected a down line: {}",
        stdout(&down)
    );
    let down_again = daemon.cli(&repo, &["down"]);
    assert_success(&down_again, "second slipfork down");
    assert!(
        stdout(&down_again).contains("nothing to do"),
        "expected idempotent second down: {}",
        stdout(&down_again)
    );

    // --- `-pgid` reaping: the component's whole host group is gone ----------
    poll_until(Duration::from_secs(30), POLL_INTERVAL, || {
        let survivors = group_members(pgid);
        if survivors.is_empty() {
            Ok(())
        } else {
            Err(format!(
                "component process group {pgid} still has live members after down: {survivors:?}"
            ))
        }
    })
    .unwrap_or_else(|last| panic!("{last}"));

    // --- subvolume reaping: `down` leaves no world behind -------------------
    assert_eq!(
        world_subvolumes(&daemon.state_dir),
        Vec::<PathBuf>::new(),
        "down must destroy every world subvolume it booted"
    );

    // --- daemon teardown reaps its socket, worlds, and process groups ------
    daemon.shutdown();
    assert!(
        !support::daemon_is_served(&daemon.runtime_dir),
        "teardown unlinks the control socket"
    );
    daemon.assert_no_world_residue("after the full world lifecycle journey");
    assert_eq!(
        world_subvolumes(&daemon.state_dir),
        Vec::<PathBuf>::new(),
        "no world subvolume may survive the journey"
    );
    assert!(
        group_members(pgid).is_empty(),
        "no component process may survive the journey"
    );
}

/// The `api` component's own pid (its launcher), read from the same `--json ps` projection.
fn host_pgrp_pid(daemon: &Daemon, cwd: &Path) -> u32 {
    let document = ps_json(daemon, cwd);
    document["services"]
        .as_array()
        .expect("services array")
        .iter()
        .find(|service| service["id"] == "api")
        .and_then(|service| service["pid"].as_u64())
        .map(|pid| u32::try_from(pid).expect("a pid"))
        .expect("the api component has a live pid")
}
