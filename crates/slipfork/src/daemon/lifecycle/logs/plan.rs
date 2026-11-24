//! Target resolution for [`super::LogStream`]: live pods, captured files, and recorded runs.

use std::fs;
use std::path::{Path, PathBuf};

use serde_json::Value;
use slipfork_forkfile::Forkfile;
use slipfork_world::LogStore;
use slipfork_world::run_log::RunRecord;

use super::super::super::Failure;
use super::super::super::rpc::key_of;
use super::super::core::{Core, World};
use super::super::stream::{SHELLABLE, load_document, project_param, world_rows};
use super::{LogStream, RunIndex, log_store_of};
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

/// One `logs <target>` decision, shared by the live and stopped-world resolvers.
enum TargetPlan {
    /// A running pod with this id streams the world socket.
    Live,
    /// `<store>/logs/<world>/<id>.log` replays through the window reader.
    Capture,
    /// No pod capture exists, but this catalog run does.
    Run(Box<RunRecord>),
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
            return Self::Run(Box::new(record.clone()));
        }
        // A bare run id (or `last`/`failed`) is a documented target of its own.
        let bare = target.trim_start_matches('#');
        if let Some(record) = index.by_id(bare) {
            return Self::Run(Box::new(record.clone()));
        }
        if matches!(bare, "last" | "failed")
            && let Ok(run_id) = store.resolve_run(bare)
            && let Some(record) = index.by_id(&run_id)
        {
            return Self::Run(Box::new(record.clone()));
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
            let run_id = store.resolve_run(spec).map_err(|_| {
                let key = spec.trim_start_matches('#');
                let (code, line, next) = match key {
                    "failed" => (
                        "no-failed-run",
                        "✗ no failed run in this world (see slipfork logs --runs)".to_owned(),
                        "slipfork up",
                    ),
                    "last" => (
                        "no-run",
                        "✗ no run recorded in this world".to_owned(),
                        "slipfork up",
                    ),
                    _ => (
                        "unknown-run",
                        format!("✗ no recorded run `{key}` in this world"),
                        "slipfork logs --runs",
                    ),
                };
                Failure::new(code, format!("{line}\nnext: {next}"))
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
                    Some(record),
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
                        Some(record),
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
