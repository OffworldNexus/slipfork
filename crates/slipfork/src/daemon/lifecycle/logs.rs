//! `logs`: one plan struct for the live world stream and the indexed capture-file replay.
//!
//! Bare `slipfork logs` merges application service captures; `-a` adds images and pod-backed
//! resources; explicit targets (`slipfork logs api worker`) merge exactly those ids. A live target
//! with its own pod streams the world socket, while an image id without a pod (or a world that is
//! not live) replays `<store>/logs/<world>/<id>.log` through the `.idx`-backed window reader, so
//! `--since`/`--until` seek instead of scanning from byte zero.

use std::fs;
use std::path::{Path, PathBuf};
use std::thread;
use std::time::Duration;

use serde_json::{Value, json};
use slipfork_forkfile::Forkfile;
use slipfork_world::control::{PROTOCOL_VERSION, Request, StreamClient, StreamFrame};
use slipfork_world::pod_log::{LogKind, LogMerge, REORDER_MS, now_ms, read_range};
use slipfork_world::run_log::{self, runs_dir};
use slipfork_world::{StoreLayout, WorldName};

use super::super::Failure;
use super::super::rpc::key_of;
use super::core::{Core, World};
use super::stream::{SHELLABLE, load_document, project_param, world_rows};
use crate::daemon::registry::RegistryEntry;
use crate::ps::PsKind;

/// Kinds bare `slipfork logs` merges by default (application services only).
const SERVICES: &[PsKind] = &[PsKind::Service];
/// Kinds `slipfork logs -a` merges (services plus pod-backed resources).
const ALL_LOGS: &[PsKind] = &[
    PsKind::Service,
    PsKind::Image,
    PsKind::Postgres,
    PsKind::Redis,
];

/// One resolved log plan: a live world stream or a capture replay.
pub(crate) struct LogStream {
    /// Live world's pod root; `None` replays capture files instead.
    pods_dir: Option<PathBuf>,
    /// `<store>/logs/<world>` capture directory used by replay.
    logs_dir: PathBuf,
    /// Capture ids to replay (already filtered by `all` unless a target was explicit).
    replay_ids: Vec<String>,
    /// Gutter roster the CLI fixes its column width from.
    roster: Vec<String>,
    /// Explicit service id for a live single-pod stream.
    service: Option<String>,
    /// Explicit pod ids for a live multi-pod stream.
    pods: Option<Vec<String>>,
    /// Keep the stream attached (`-f`).
    follow: bool,
    /// Lower bound in epoch milliseconds.
    since_ms: Option<u64>,
    /// Upper bound in epoch milliseconds.
    until_ms: Option<u64>,
    /// The id came from the command line, so a missing capture file is an error.
    explicit: bool,
    /// A recorded-run catalog listing or one run replay, instead of pod captures.
    runs: Option<RunQuery>,
}

/// What `logs --runs` / `logs -r <id>` asked for.
enum RunQuery {
    /// List every catalog entry (`--runs`).
    List,
    /// Replay one run spec (`<id>`, `#<id>`, `last`, `failed`).
    Replay(String),
}

impl Core {
    /// Resolves one `logs` request into a plan; the caller runs it without holding the core lock
    /// so a followed stream never blocks a concurrent `up`/`down`.
    pub(crate) fn plan_logs_stream(&mut self, params: &Value) -> Result<LogStream, Failure> {
        let project = project_param(params)?;
        let all = params.get("all").and_then(Value::as_bool).unwrap_or(false);
        let targets: Vec<String> = params
            .get("targets")
            .and_then(Value::as_array)
            .map(|items| {
                items
                    .iter()
                    .filter_map(Value::as_str)
                    .map(str::to_owned)
                    .collect()
            })
            .unwrap_or_default();
        let follow = params
            .get("follow")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        let since_ms = params.get("since_ms").and_then(Value::as_u64);
        let until_ms = params.get("until_ms").and_then(Value::as_u64);
        if params.get("runs").and_then(Value::as_bool).unwrap_or(false) {
            return Ok(LogStream::run_catalog(self.logs_dir(&project)?));
        }
        if let Some(spec) = params.get("run").and_then(Value::as_str) {
            return Ok(LogStream::run_replay(
                self.logs_dir(&project)?,
                spec.to_owned(),
                since_ms,
                until_ms,
            ));
        }
        let document = load_document(&project);
        self.plan_logs(
            &project,
            document.as_ref(),
            targets,
            follow,
            all,
            since_ms,
            until_ms,
        )
    }

    #[allow(clippy::too_many_arguments)]
    fn plan_logs(
        &mut self,
        project: &Path,
        document: Option<&Forkfile>,
        targets: Vec<String>,
        follow: bool,
        all: bool,
        since_ms: Option<u64>,
        until_ms: Option<u64>,
    ) -> Result<LogStream, Failure> {
        let Some((pods_dir, _world, rows)) = self.live_world(project)? else {
            return self
                .capture_stream(project, document, targets, follow, all, since_ms, until_ms);
        };
        if targets.is_empty() {
            let kinds = if all { ALL_LOGS } else { SERVICES };
            let ids: Vec<String> = rows
                .iter()
                .filter(|(kind, _, running)| kinds.contains(kind) && *running)
                .map(|(_, id, _)| id.clone())
                .collect();
            if ids.is_empty() {
                return self
                    .capture_stream(project, document, targets, follow, all, since_ms, until_ms);
            }
            return Ok(LogStream::live(
                pods_dir,
                ids.clone(),
                None,
                Some(ids),
                follow,
                since_ms,
                until_ms,
            ));
        }
        if let Some(id) = targets.iter().find(|target| {
            rows.iter()
                .any(|(kind, id, _)| id == *target && *kind == PsKind::S3)
        }) {
            return Err(s3_refusal(id, &shellable(&rows)));
        }
        let capture_dir = self.logs_dir(project)?;
        let captured = capture_ids(&capture_dir);
        for target in &targets {
            let known = rows.iter().any(|(_, id, _)| id == target)
                || captured.iter().any(|id| id == target);
            if !known {
                return Err(unknown_target(target));
            }
        }
        if targets.len() == 1 {
            let target = targets[0].clone();
            if let Some((kind, _, true)) = rows.iter().find(|(_, id, _)| id == &target)
                && SHELLABLE.contains(kind)
            {
                return Ok(LogStream::live(
                    pods_dir,
                    vec![target.clone()],
                    Some(target),
                    None,
                    follow,
                    since_ms,
                    until_ms,
                ));
            }
            return self
                .capture_stream(project, document, targets, follow, all, since_ms, until_ms);
        }
        Ok(LogStream::live(
            pods_dir,
            targets.clone(),
            None,
            Some(targets),
            follow,
            since_ms,
            until_ms,
        ))
    }

    /// A replay plan over `<store>/logs/<world>`; bare `logs` keeps service captures only.
    #[allow(clippy::too_many_arguments)]
    fn capture_stream(
        &self,
        project: &Path,
        document: Option<&Forkfile>,
        targets: Vec<String>,
        follow: bool,
        all: bool,
        since_ms: Option<u64>,
        until_ms: Option<u64>,
    ) -> Result<LogStream, Failure> {
        let dir = self.logs_dir(project)?;
        let (ids, explicit) = if targets.is_empty() {
            let mut ids = capture_ids(&dir);
            if !all {
                let services = service_ids(document);
                ids.retain(|id| services.contains(id));
            }
            (ids, false)
        } else {
            (targets, true)
        };
        Ok(LogStream::captures(
            dir, ids, explicit, follow, since_ms, until_ms,
        ))
    }

    /// The canonical `<store>/logs/<world>` directory, live or stopped.
    fn logs_dir(&self, project: &Path) -> Result<PathBuf, Failure> {
        let world = match self.worlds.get(&key_of(project)) {
            Some(World::Live { entry, .. }) => entry.world.clone(),
            _ => crate::world::world_name(project, None)
                .map_err(|error| Failure::new("no-world", error))?
                .as_str()
                .to_owned(),
        };
        let layout = StoreLayout::at(self.host.store_root_for(project));
        let parsed = WorldName::parse(&world)
            .map_err(|error| Failure::new("no-world", error.to_string()))?;
        Ok(layout.world_logs(&parsed))
    }

    /// The live world's pod root, name, and `(kind, id, running)` roster.
    #[allow(clippy::type_complexity)]
    fn live_world(
        &mut self,
        project: &Path,
    ) -> Result<Option<(PathBuf, String, Vec<(PsKind, String, bool)>)>, Failure> {
        let Some(entry) = self.live_entry(project) else {
            return Ok(None);
        };
        let pods_dir = entry.pods_dir.clone();
        let world = entry.world.clone();
        let reply = self.ps()?;
        let mut rows = Vec::new();
        for row in world_rows(&reply, project) {
            for service in &row.services {
                rows.push((service.kind, service.id.clone(), service.running));
            }
        }
        Ok(Some((pods_dir, world, rows)))
    }

    fn live_entry(&self, project: &Path) -> Option<&RegistryEntry> {
        match self.worlds.get(&key_of(project)) {
            Some(World::Live { entry, .. }) => Some(entry),
            _ => None,
        }
    }
}

impl LogStream {
    #[allow(clippy::too_many_arguments)]
    fn live(
        pods_dir: PathBuf,
        roster: Vec<String>,
        service: Option<String>,
        pods: Option<Vec<String>>,
        follow: bool,
        since_ms: Option<u64>,
        until_ms: Option<u64>,
    ) -> Self {
        Self {
            pods_dir: Some(pods_dir),
            logs_dir: PathBuf::new(),
            replay_ids: Vec::new(),
            roster,
            service,
            pods,
            follow,
            since_ms,
            until_ms,
            explicit: false,
            runs: None,
        }
    }

    fn captures(
        logs_dir: PathBuf,
        replay_ids: Vec<String>,
        explicit: bool,
        follow: bool,
        since_ms: Option<u64>,
        until_ms: Option<u64>,
    ) -> Self {
        Self {
            pods_dir: None,
            logs_dir,
            replay_ids,
            roster: Vec::new(),
            service: None,
            pods: None,
            follow,
            since_ms,
            until_ms,
            explicit,
            runs: None,
        }
    }

    /// A catalog listing of every recorded step run (`logs --runs`).
    fn run_catalog(logs_dir: PathBuf) -> Self {
        Self {
            runs: Some(RunQuery::List),
            ..Self::captures(logs_dir, Vec::new(), false, false, None, None)
        }
    }

    /// A replay plan for one recorded run spec (`logs -r <id>|last|failed`).
    fn run_replay(
        logs_dir: PathBuf,
        spec: String,
        since_ms: Option<u64>,
        until_ms: Option<u64>,
    ) -> Self {
        Self {
            runs: Some(RunQuery::Replay(spec)),
            ..Self::captures(logs_dir, Vec::new(), true, false, since_ms, until_ms)
        }
    }

    pub(crate) fn run(
        &self,
        emit: &mut dyn FnMut(Value) -> bool,
        alive: &mut dyn FnMut() -> bool,
    ) -> Result<Value, Failure> {
        match &self.runs {
            Some(RunQuery::List) => self.list_runs(emit),
            Some(RunQuery::Replay(spec)) => self.replay_run(spec, emit, alive),
            None => match &self.pods_dir {
                Some(pods_dir) => self.stream_pods(pods_dir, emit, alive),
                None => self.read_captures(emit, alive),
            },
        }
    }

    /// Emits one `runs` frame carrying every catalog entry, newest last.
    fn list_runs(&self, emit: &mut dyn FnMut(Value) -> bool) -> Result<Value, Failure> {
        let records = run_log::read_index(&self.logs_dir).map_err(|_| {
            Failure::new(
                "run-index",
                format!(
                    "✗ cannot read `{}`\n  next: remove the malformed line and retry",
                    runs_dir(&self.logs_dir).join(run_log::INDEX_FILE).display()
                ),
            )
        })?;
        let rows: Vec<Value> = records
            .iter()
            .map(|record| {
                json!({
                    "run_id": record.run_id,
                    "ts_ms": record.ts_ms,
                    "kind": record.kind,
                    "name": record.name,
                    "command": record.command,
                    "state": record.state,
                    "duration_ms": record.duration_ms,
                })
            })
            .collect();
        emit(json!({ "event": "runs", "rows": rows }));
        Ok(json!({ "runs": records.len() }))
    }

    /// Replays one run's framed capture through the indexed window reader.
    fn replay_run(
        &self,
        spec: &str,
        emit: &mut dyn FnMut(Value) -> bool,
        alive: &mut dyn FnMut() -> bool,
    ) -> Result<Value, Failure> {
        let runs_dir = runs_dir(&self.logs_dir);
        let run_id = run_log::resolve_run(&self.logs_dir, spec).map_err(|error| {
            Failure::new(
                "no-run",
                format!("✗ {error}\n  next: list recorded runs with `slipfork logs --runs`"),
            )
        })?;
        let mut reader =
            read_range(&runs_dir, &run_id, self.since_ms, self.until_ms).map_err(|error| {
                Failure::new("no-run", format!("✗ cannot read run `{run_id}`: {error}"))
            })?;
        if !emit(json!({
            "kind": "roster",
            "pod": run_id,
            "stream": "combined",
            "line": "",
            "pods": [run_id],
        })) {
            return Ok(json!({ "closed": true }));
        }
        loop {
            let mut lines = Vec::new();
            let _ = reader.read_lines(&mut |line| lines.push(line.clone()));
            for line in lines {
                emit(line_event(
                    &run_id,
                    kind_name(line.kind),
                    &line.text,
                    Some(line.ts_ms),
                ));
            }
            if !self.follow || !alive() {
                let mut rest = Vec::new();
                reader.flush(&mut |line| rest.push(line.clone()));
                for line in rest {
                    emit(line_event(
                        &run_id,
                        kind_name(line.kind),
                        &line.text,
                        Some(line.ts_ms),
                    ));
                }
                return Ok(json!({ "closed": false }));
            }
            thread::sleep(Duration::from_millis(40));
        }
    }

    /// Streams the world's captured output for the resolved ids; the roster fixes the gutter.
    fn stream_pods(
        &self,
        pods_dir: &Path,
        emit: &mut dyn FnMut(Value) -> bool,
        alive: &mut dyn FnMut() -> bool,
    ) -> Result<Value, Failure> {
        let mut client = StreamClient::open(pods_dir)
            .map_err(|error| Failure::new("world-unavailable", error.to_string()))?;
        client
            .send(&Request::Logs {
                v: PROTOCOL_VERSION,
                id: "daemon-logs".to_owned(),
                service: self.service.clone(),
                pods: self.pods.clone(),
                follow: self.follow,
                since_ms: self.since_ms,
                until_ms: self.until_ms,
            })
            .map_err(|error| Failure::new("world-unavailable", error.to_string()))?;
        if !emit(json!({
            "kind": "roster",
            "pod": "",
            "stream": "combined",
            "line": "",
            "pods": self.roster,
        })) {
            client.close();
            return Ok(json!({ "closed": true }));
        }
        loop {
            if !alive() {
                client.close();
                return Ok(json!({ "closed": true }));
            }
            match client.poll() {
                Ok(Some(StreamFrame::Output(event))) => {
                    if !emit(line_event(
                        &event.pod,
                        event.kind.as_str(),
                        &event.line,
                        event.ts_ms,
                    )) {
                        client.close();
                        return Ok(json!({ "closed": true }));
                    }
                }
                Ok(Some(StreamFrame::Reply { ok, error })) => {
                    return super::stream::finish(ok, error);
                }
                Ok(None) => thread::sleep(Duration::from_millis(40)),
                Err(error) => return Err(Failure::new("world-stream", error.to_string())),
            }
        }
    }

    /// Replays `<store>/logs/<world>/<id>.log` through the indexed window reader.
    fn read_captures(
        &self,
        emit: &mut dyn FnMut(Value) -> bool,
        alive: &mut dyn FnMut() -> bool,
    ) -> Result<Value, Failure> {
        let mut readers = Vec::new();
        for id in &self.replay_ids {
            match read_range(&self.logs_dir, id, self.since_ms, self.until_ms) {
                Ok(reader) => readers.push((id.clone(), reader)),
                Err(_) if !self.explicit => {}
                Err(error) => {
                    return Err(Failure::new(
                        "no-log",
                        format!(
                            "no captured logs for `{id}` in `{}` ({error})",
                            self.logs_dir.display()
                        ),
                    ));
                }
            }
        }
        if readers.is_empty() {
            return Err(Failure::new(
                "no-target",
                format!("no captured logs in `{}`", self.logs_dir.display()),
            ));
        }
        let roster: Vec<String> = readers.iter().map(|(id, _)| id.clone()).collect();
        if !emit(json!({
            "kind": "roster",
            "pod": "",
            "stream": "combined",
            "line": "",
            "pods": roster,
        })) {
            return Ok(json!({ "closed": true }));
        }
        let mut merge = LogMerge::default();
        loop {
            let mut read_any = false;
            for (id, reader) in &mut readers {
                let _ = reader.read_lines(&mut |line| {
                    read_any = true;
                    merge.push(id, line);
                });
            }
            for (pod, line) in merge.drain(now_ms().saturating_sub(REORDER_MS)) {
                emit(line_event(
                    &pod,
                    kind_name(line.kind),
                    &line.text,
                    Some(line.ts_ms),
                ));
            }
            if !self.follow {
                for (id, reader) in &mut readers {
                    reader.flush(&mut |line| merge.push(id, line));
                }
                for (pod, line) in merge.flush() {
                    emit(line_event(
                        &pod,
                        kind_name(line.kind),
                        &line.text,
                        Some(line.ts_ms),
                    ));
                }
                return Ok(json!({ "closed": false }));
            }
            if !alive() {
                return Ok(json!({ "closed": true }));
            }
            if !read_any {
                thread::sleep(Duration::from_millis(40));
            }
        }
    }
}

/// One daemon event carrying `ts_ms` only when the capture knew it.
fn line_event(pod: &str, kind: &str, line: &str, ts_ms: Option<u64>) -> Value {
    let mut event = json!({
        "kind": kind,
        "pod": pod,
        "stream": "combined",
        "line": line,
    });
    if let Some(ts_ms) = ts_ms {
        event["ts_ms"] = json!(ts_ms);
    }
    event
}

fn kind_name(kind: LogKind) -> &'static str {
    match kind {
        LogKind::Line => "line",
        LogKind::Divider => "divider",
    }
}

/// Running shellable ids a remedy can name, in roster order.
fn shellable(rows: &[(PsKind, String, bool)]) -> Vec<String> {
    let mut ids = Vec::new();
    for (kind, id, running) in rows {
        if SHELLABLE.contains(kind) && *running && !ids.contains(id) {
            ids.push(id.clone());
        }
    }
    ids
}

/// `logs <s3>`: one line plus the one actionable `next:`, printed verbatim by the CLI.
fn s3_refusal(id: &str, shellable: &[String]) -> Failure {
    let next = shellable.first().map_or_else(
        || "slipfork up".to_owned(),
        |target| format!("slipfork shell {target}"),
    );
    Failure::new(
        "shell-refused",
        format!("✗ native S3 bucket '{id}' has no pod logs\n  next: {next}"),
    )
}

/// An explicit id that is neither in the world roster nor backed by a capture file.
fn unknown_target(id: &str) -> Failure {
    Failure::new(
        "no-log",
        format!("✗ '{id}' is not running and has no captured logs"),
    )
}

fn service_ids(document: Option<&Forkfile>) -> Vec<String> {
    document
        .map(|document| {
            document
                .services()
                .into_iter()
                .map(|service| service.id.to_owned())
                .collect()
        })
        .unwrap_or_default()
}

/// Every `<id>.log` stem in one world's capture directory, sorted.
fn capture_ids(dir: &Path) -> Vec<String> {
    let mut ids: Vec<String> = fs::read_dir(dir)
        .map(|entries| {
            entries
                .flatten()
                .filter_map(|entry| {
                    let name = entry.file_name().into_string().ok()?;
                    name.strip_suffix(".log").map(str::to_owned)
                })
                .collect()
        })
        .unwrap_or_default();
    ids.sort();
    ids
}
