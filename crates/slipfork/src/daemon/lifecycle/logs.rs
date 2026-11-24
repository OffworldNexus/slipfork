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
use slipfork_world::LogStore;
use slipfork_world::control::{PROTOCOL_VERSION, Request, StreamClient, StreamFrame};
use slipfork_world::pod_log::{LogKind, LogMerge, REORDER_MS, now_ms, read_range};
use slipfork_world::run_log::RunRecord;

use super::super::Failure;
use super::super::rpc::key_of;
use super::core::{Core, World};
use super::stream::{SHELLABLE, load_document, project_param, world_rows};
use crate::daemon::registry::RegistryEntry;
use crate::ps::PsKind;
use crate::world::features::WorldFeatures;

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
    /// The resolved world log store used by replay; `None` for a live stream.
    store: Option<LogStore>,
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
    /// The one catalog snapshot this request resolves every target from; `None` when no target
    /// asked for it (bare `logs`, a live stream, or a catalog listing).
    catalog: Option<RunIndex>,
}

/// What `logs --runs` / `logs -r <id>` asked for.
enum RunQuery {
    /// List every catalog entry (`--runs`).
    List,
    /// Replay one already-resolved run id, with its catalog record for `--json`.
    Replay {
        run_id: String,
        record: Option<Box<RunRecord>>,
    },
}

/// One catalog read shared by every target and every replay in one request.
struct RunIndex {
    records: Vec<RunRecord>,
}

impl RunIndex {
    fn load(store: &LogStore) -> Result<Self, Failure> {
        let catalog = store
            .records()
            .map_err(|error| catalog_failure(store, &error))?;
        Ok(Self {
            records: catalog.records,
        })
    }

    /// The newest record whose `target` matches, whatever its kind.
    fn newest_for_target(&self, target: &str) -> Option<&RunRecord> {
        self.records
            .iter()
            .filter(|record| record.target == target)
            .max_by_key(|record| record.started_ms)
    }

    fn by_id(&self, id: &str) -> Option<&RunRecord> {
        self.records.iter().find(|record| record.run_id == id)
    }
}

/// One `logs <target>` decision, shared by the live and stopped-world resolvers.
enum TargetPlan {
    /// A running pod with this id streams the world socket.
    Live,
    /// `<store>/logs/<world>/<id>.log` replays through the window reader.
    Capture,
    /// No pod capture exists, but this catalog run does.
    Run(RunRecord),
    /// Neither a live pod, a capture, nor a recorded run.
    Unknown,
}

impl TargetPlan {
    fn resolve(
        store: &LogStore,
        index: &RunIndex,
        captured: &[String],
        live: &[(PsKind, String, bool)],
        target: &str,
    ) -> Self {
        if live
            .iter()
            .any(|(kind, id, running)| id == target && *running && SHELLABLE.contains(kind))
        {
            return Self::Live;
        }
        if captured.iter().any(|id| id == target) {
            return Self::Capture;
        }
        if let Some(record) = index.newest_for_target(target) {
            return Self::Run(record.clone());
        }
        // A bare run id (or `last`/`failed`) is a documented target of its own.
        let bare = target.trim_start_matches('#');
        if let Some(record) = index.by_id(bare) {
            return Self::Run(record.clone());
        }
        if matches!(bare, "last" | "failed")
            && let Ok(run_id) = store.resolve_run(bare)
            && let Some(record) = index.by_id(&run_id)
        {
            return Self::Run(record.clone());
        }
        // A declared-but-stopped pod still owns the id, so a capture read fails with its one remedy.
        if live.iter().any(|(_, id, _)| id == target) {
            return Self::Capture;
        }
        Self::Unknown
    }
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
            return Ok(LogStream::run_catalog(log_store_of(self, &project)?));
        }
        if let Some(spec) = params.get("run").and_then(Value::as_str) {
            let store = log_store_of(self, &project)?;
            let run_id = store.resolve_run(spec).map_err(|error| {
                Failure::new(
                    "no-run",
                    format!("✗ {error}\nnext: list recorded runs with `slipfork logs --runs`"),
                )
            })?;
            let record = RunIndex::load(&store)?
                .by_id(&run_id)
                .cloned()
                .map(Box::new);
            return Ok(LogStream::run_replay(
                store, run_id, record, since_ms, until_ms,
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
        let store = log_store_of(self, project)?;
        let captured = capture_ids(&store.world_logs());
        let index = RunIndex::load(&store)?;
        let plans: Vec<TargetPlan> = targets
            .iter()
            .map(|target| TargetPlan::resolve(&store, &index, &captured, &rows, target))
            .collect();
        if let Some(target) = plans
            .iter()
            .zip(&targets)
            .find_map(|(plan, target)| matches!(plan, TargetPlan::Unknown).then_some(target))
        {
            return Err(unknown_target(target));
        }
        if targets.len() == 1 {
            return match plans.into_iter().next().unwrap_or(TargetPlan::Unknown) {
                TargetPlan::Live => Ok(LogStream::live(
                    pods_dir,
                    targets.clone(),
                    Some(targets[0].clone()),
                    None,
                    follow,
                    since_ms,
                    until_ms,
                )),
                // A service that never opened a pod (a failed build or `setup`) has no `<id>.log`,
                // but its run capture does: replay through the one run path.
                TargetPlan::Run(record) => Ok(LogStream::run_replay(
                    store,
                    record.run_id.clone(),
                    Some(Box::new(record)),
                    since_ms,
                    until_ms,
                )),
                TargetPlan::Capture | TargetPlan::Unknown => self.capture_stream_of(
                    project,
                    document,
                    store,
                    captured,
                    Some(index),
                    targets,
                    follow,
                    all,
                    since_ms,
                    until_ms,
                ),
            };
        }
        // A live pod cannot be merged with a capture replay, so any live target keeps the socket
        // stream exactly as before; only an all-replay set falls through to the capture merge.
        if plans.iter().any(|plan| matches!(plan, TargetPlan::Live)) {
            return Ok(LogStream::live(
                pods_dir,
                targets.clone(),
                None,
                Some(targets),
                follow,
                since_ms,
                until_ms,
            ));
        }
        self.capture_stream_of(
            project,
            document,
            store,
            captured,
            Some(index),
            targets,
            follow,
            all,
            since_ms,
            until_ms,
        )
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
        let store = log_store_of(self, project)?;
        let captured = capture_ids(&store.world_logs());
        let index = if targets.is_empty() {
            None
        } else {
            Some(RunIndex::load(&store)?)
        };
        self.capture_stream_of(
            project, document, store, captured, index, targets, follow, all, since_ms, until_ms,
        )
    }

    /// Builds the capture plan from one already-loaded store, capture list, and catalog snapshot.
    #[allow(clippy::too_many_arguments)]
    fn capture_stream_of(
        &self,
        project: &Path,
        document: Option<&Forkfile>,
        store: LogStore,
        captured: Vec<String>,
        index: Option<RunIndex>,
        targets: Vec<String>,
        follow: bool,
        all: bool,
        since_ms: Option<u64>,
        until_ms: Option<u64>,
    ) -> Result<LogStream, Failure> {
        if targets.is_empty() {
            let mut ids = captured;
            if !all {
                let services = service_ids(project, document)?;
                ids.retain(|id| services.contains(id));
            }
            let stream = LogStream::captures(store, ids, false, follow, since_ms, until_ms);
            return Ok(match index {
                Some(index) => stream.with_catalog(index),
                None => stream,
            });
        }
        let Some(index) = index else {
            return Ok(LogStream::captures(
                store, targets, true, follow, since_ms, until_ms,
            ));
        };
        if targets.len() == 1 {
            let target = &targets[0];
            match TargetPlan::resolve(&store, &index, &captured, &[], target) {
                TargetPlan::Run(record) => {
                    return Ok(LogStream::run_replay(
                        store,
                        record.run_id.clone(),
                        Some(Box::new(record)),
                        since_ms,
                        until_ms,
                    ));
                }
                TargetPlan::Unknown => return Err(unknown_target(target)),
                TargetPlan::Live | TargetPlan::Capture => {}
            }
        } else {
            for target in &targets {
                if matches!(
                    TargetPlan::resolve(&store, &index, &captured, &[], target),
                    TargetPlan::Unknown
                ) {
                    return Err(unknown_target(target));
                }
            }
        }
        Ok(
            LogStream::captures(store, targets, true, follow, since_ms, until_ms)
                .with_catalog(index),
        )
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
            store: None,
            replay_ids: Vec::new(),
            roster,
            service,
            pods,
            follow,
            since_ms,
            until_ms,
            explicit: false,
            runs: None,
            catalog: None,
        }
    }

    fn captures(
        store: LogStore,
        replay_ids: Vec<String>,
        explicit: bool,
        follow: bool,
        since_ms: Option<u64>,
        until_ms: Option<u64>,
    ) -> Self {
        Self {
            pods_dir: None,
            store: Some(store),
            replay_ids,
            roster: Vec::new(),
            service: None,
            pods: None,
            follow,
            since_ms,
            until_ms,
            explicit,
            runs: None,
            catalog: None,
        }
    }

    /// Attaches the one catalog snapshot this request resolved its targets from.
    fn with_catalog(mut self, catalog: RunIndex) -> Self {
        self.catalog = Some(catalog);
        self
    }

    /// A catalog listing of every recorded step run (`logs --runs`).
    fn run_catalog(store: LogStore) -> Self {
        Self {
            runs: Some(RunQuery::List),
            ..Self::captures(store, Vec::new(), false, false, None, None)
        }
    }

    /// A replay plan for one already-resolved run id, with its catalog record for `--json`.
    fn run_replay(
        store: LogStore,
        run_id: String,
        record: Option<Box<RunRecord>>,
        since_ms: Option<u64>,
        until_ms: Option<u64>,
    ) -> Self {
        Self {
            runs: Some(RunQuery::Replay { run_id, record }),
            ..Self::captures(store, Vec::new(), true, false, since_ms, until_ms)
        }
    }

    pub(crate) fn run(
        &self,
        emit: &mut dyn FnMut(Value) -> bool,
        alive: &mut dyn FnMut() -> bool,
    ) -> Result<Value, Failure> {
        match &self.runs {
            Some(RunQuery::List) => self.list_runs(emit),
            Some(RunQuery::Replay { run_id, record }) => {
                self.replay_run(run_id, record.as_deref(), emit, alive)
            }
            None => match &self.pods_dir {
                Some(pods_dir) => self.stream_pods(pods_dir, emit, alive),
                None => self.read_captures(emit, alive),
            },
        }
    }

    /// Emits one `runs` frame carrying every catalog entry, newest last.
    fn list_runs(&self, emit: &mut dyn FnMut(Value) -> bool) -> Result<Value, Failure> {
        let store = self.store.as_ref().ok_or_else(|| {
            Failure::new("no-world", "✗ no world log store\nnext: run `slipfork up`")
        })?;
        let catalog = store.records().map_err(|error| {
            Failure::new(
                "run-index",
                format!(
                    "✗ cannot read `{}`: {error}\nnext: check that `{}` is readable",
                    store.index().display(),
                    store.run_dir().display()
                ),
            )
        })?;
        let rows: Vec<Value> = catalog
            .records
            .iter()
            .filter_map(|record| serde_json::to_value(record).ok())
            .collect();
        emit(json!({ "event": "runs", "rows": rows, "skipped": catalog.skipped }));
        Ok(json!({ "runs": catalog.records.len(), "skipped": catalog.skipped }))
    }

    /// Replays one already-resolved run through the indexed window reader.
    fn replay_run(
        &self,
        run_id: &str,
        record: Option<&RunRecord>,
        emit: &mut dyn FnMut(Value) -> bool,
        alive: &mut dyn FnMut() -> bool,
    ) -> Result<Value, Failure> {
        let store = self.store.as_ref().ok_or_else(|| {
            Failure::new("no-world", "✗ no world log store\nnext: run `slipfork up`")
        })?;
        let mut reader = read_range(&store.run_dir(), run_id, self.since_ms, self.until_ms)
            .map_err(|error| {
                Failure::new(
                    "no-run",
                    format!(
                        "✗ cannot read run `{run_id}`: {error}\nnext: retry with `slipfork logs -r last`"
                    ),
                )
            })?;
        // `-r --json` mirrors the catalog record the replay belongs to, then its captured lines.
        let mut roster = json!({
            "kind": "roster",
            "pod": run_id,
            "stream": "combined",
            "line": "",
            "pods": [run_id],
        });
        if let Some(record) = record
            && let Ok(value) = serde_json::to_value(record)
        {
            roster["run"] = value;
        }
        if !emit(roster) {
            return Ok(json!({ "closed": true }));
        }
        loop {
            let mut lines = Vec::new();
            let _ = reader.read_lines(&mut |line| lines.push(line.clone()));
            for line in lines {
                emit(line_event(
                    run_id,
                    kind_name(line.kind),
                    line.stream.as_str(),
                    &line.text,
                    Some(line.ts_ms),
                ));
            }
            if !self.follow || !alive() {
                let mut rest = Vec::new();
                reader.flush(&mut |line| rest.push(line.clone()));
                for line in rest {
                    emit(line_event(
                        run_id,
                        kind_name(line.kind),
                        line.stream.as_str(),
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
                        event.stream.as_str(),
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
        let Some(store) = &self.store else {
            return Err(Failure::new(
                "no-target",
                "✗ no world log store\nnext: run `slipfork up`",
            ));
        };
        let logs_dir = store.world_logs();
        // A single image id replays its own run capture, so its header and its lines always come
        // from the same run instead of a stale append-only `<image>.log`.
        let image_run = (self.replay_ids.len() == 1)
            .then(|| {
                self.catalog
                    .as_ref()
                    .and_then(|index| index.newest_for_target(&self.replay_ids[0]))
                    .filter(|record| record.kind == "image")
                    .cloned()
            })
            .flatten();
        let mut readers = Vec::new();
        for id in &self.replay_ids {
            if let Some(record) = &image_run {
                let reader = read_range(
                    &store.run_dir(),
                    &record.run_id,
                    self.since_ms,
                    self.until_ms,
                )
                .map_err(|error| {
                    Failure::new(
                        "no-log",
                        format!(
                            "✗ cannot read run `{}` for `{id}`: {error}\nnext: retry with `slipfork logs -r last`",
                            record.run_id
                        ),
                    )
                })?;
                readers.push((id.clone(), reader));
                continue;
            }
            match read_range(&logs_dir, id, self.since_ms, self.until_ms) {
                Ok(reader) => readers.push((id.clone(), reader)),
                Err(_) if !self.explicit => {}
                Err(error) => {
                    // A failed build/hook wrote its one capture under `runs/`, not a live
                    // `<world_logs>/<id>.log`; replay the request's catalog record for this target.
                    if let Some(record) = self
                        .catalog
                        .as_ref()
                        .and_then(|index| index.newest_for_target(id))
                        && let Ok(reader) = read_range(
                            &store.run_dir(),
                            &record.run_id,
                            self.since_ms,
                            self.until_ms,
                        )
                    {
                        readers.push((id.clone(), reader));
                        continue;
                    }
                    return Err(Failure::new(
                        "no-log",
                        format!(
                            "✗ no captured logs for `{id}` in `{}` ({error})\nnext: check that `{}` is readable",
                            logs_dir.display(),
                            logs_dir.display()
                        ),
                    ));
                }
            }
        }
        if readers.is_empty() {
            return Err(Failure::new(
                "no-target",
                format!(
                    "✗ no captured logs in `{}`\nnext: run `slipfork up`",
                    logs_dir.display()
                ),
            ));
        }
        let roster: Vec<String> = readers.iter().map(|(id, _)| id.clone()).collect();
        let mut frame = json!({
            "kind": "roster",
            "pod": "",
            "stream": "combined",
            "line": "",
            "pods": roster,
        });
        // One resolved capture still names its catalog record, so the replay's header describes the
        // run its lines came from. A service capture keeps no header.
        if let Some(record) = &image_run
            && let Ok(value) = serde_json::to_value(record)
        {
            frame["run"] = value;
        }
        if !emit(frame) {
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
                    line.stream.as_str(),
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
                        line.stream.as_str(),
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

/// One daemon event carrying `ts_ms` only when the capture knew it and always its stream.
fn line_event(pod: &str, kind: &str, stream: &str, line: &str, ts_ms: Option<u64>) -> Value {
    let mut event = json!({
        "kind": kind,
        "pod": pod,
        "stream": stream,
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
        format!("✗ native S3 bucket '{id}' has no pod logs\nnext: {next}"),
    )
}

/// An explicit id that is neither in the world roster nor backed by a capture file.
fn unknown_target(id: &str) -> Failure {
    Failure::new(
        "no-log",
        format!(
            "✗ '{id}' is not running and has no captured logs\nnext: list recorded runs with `slipfork logs --runs`"
        ),
    )
}

/// Resolves one project's canonical `<store>/logs/<world>` store, live or stopped.
fn log_store_of(core: &Core, project: &Path) -> Result<LogStore, Failure> {
    core.log_store(project).ok_or_else(|| {
        Failure::new(
            "no-world",
            format!("no world log store for `{}`", project.display()),
        )
    })
}

/// One readable refusal when the run catalog itself cannot be read.
fn catalog_failure(store: &LogStore, error: &slipfork_world::Error) -> Failure {
    Failure::new(
        "run-index",
        format!(
            "✗ cannot read `{}`: {error}\nnext: check that `{}` is readable",
            store.index().display(),
            store.run_dir().display()
        ),
    )
}

fn service_ids(project: &Path, document: Option<&Forkfile>) -> Result<Vec<String>, Failure> {
    let Some(document) = document else {
        return Ok(Vec::new());
    };
    let features = WorldFeatures::load_resolved(project, document)
        .map_err(|error| Failure::new("feature", error))?;
    Ok(document
        .services_with(&features)
        .into_iter()
        .map(|service| service.id.to_owned())
        .collect())
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

#[cfg(test)]
mod tests {
    use std::io::Write;
    use std::sync::Mutex;

    use slipfork_world::WorldName;
    use slipfork_world::run_log::RunCapture;

    use super::{LogStream, RunIndex};

    /// A catalog run replays even when no `<world_logs>/<id>.log` exists, so a failed build or
    /// hook survives `fail_boot` forgetting the world.
    #[test]
    fn a_catalog_run_replays_without_a_world_capture() {
        let root = tempfile::Builder::new()
            .prefix("logs-catalog")
            .tempdir_in("/var/tmp")
            .expect("disk temp directory");
        let store = slipfork_world::LogStore::at(
            root.path().to_path_buf(),
            WorldName::parse("demo").expect("valid world"),
        );
        let project = root.path().join("demo");
        let mut capture = RunCapture::begin(&store, &project, "image", "app", "dockerfile .")
            .expect("begin a real run capture");
        capture
            .append_stream(slipfork_world::Stream::Stdout, "failed-run-marker")
            .expect("append the line");
        let run_id = capture.run_id().to_owned();
        capture.finish("failed", 9).expect("finish the run");
        assert!(!store.world_logs().join("app.log").exists());
        // One old-schema line is counted and skipped instead of bricking the catalog.
        let mut index_file = std::fs::OpenOptions::new()
            .append(true)
            .open(store.index())
            .expect("open the catalog");
        writeln!(index_file, "{{\"legacy\":true}}").expect("append an old-schema line");
        drop(index_file);
        let index = RunIndex::load(&store).expect("load the catalog once");
        assert_eq!(
            store.records().expect("read the catalog").skipped,
            1,
            "the old-schema line is counted"
        );

        let events = Mutex::new(Vec::new());
        let outcome = LogStream::captures(
            store.clone(),
            vec!["app".to_owned()],
            true,
            false,
            None,
            None,
        )
        .with_catalog(index)
        .run(
            &mut |event| {
                events.lock().expect("record the event").push(event);
                true
            },
            &mut || true,
        )
        .expect("the catalog fallback replays");
        assert_eq!(outcome["closed"], false);
        let events = events.into_inner().expect("read the events");
        assert!(
            events
                .iter()
                .any(|event| event["line"] == "failed-run-marker" && event["pod"] == "app"),
            "{events:?}"
        );
        assert!(
            events.iter().any(|event| event["kind"] == "roster"),
            "the replay announces its roster first: {events:?}"
        );
        assert_eq!(run_id.len(), slipfork_world::run_log::RUN_ID_LEN);

        let runs = LogStream::run_catalog(store.clone())
            .run(&mut |_| true, &mut || true)
            .expect("list the catalog");
        assert_eq!(runs["runs"], 1, "{runs}");
        assert_eq!(
            runs["skipped"], 1,
            "the old-schema line is reported: {runs}"
        );

        let refused =
            LogStream::captures(store, vec!["missing".to_owned()], true, false, None, None)
                .run(&mut |_| true, &mut || true);
        assert!(
            refused.is_err(),
            "an unknown target still fails closed: {refused:?}"
        );
    }
}
