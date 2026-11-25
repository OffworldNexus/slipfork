//! `logs`: one plan struct for the live world stream and the indexed capture-file replay.
//!
//! Bare `slipfork logs` merges application service captures; `-a` adds images and pod-backed
//! resources; explicit targets (`slipfork logs api worker`) merge exactly those ids. A live target
//! with its own pod streams the world socket, while an image id without a pod (or a world that is
//! not live) replays `<store>/logs/<world>/<id>.log` through the `.idx`-backed window reader, so
//! `--since`/`--until` seek instead of scanning from byte zero.

use std::path::Path;

use serde_json::Value;
use slipfork_world::LogStore;
use slipfork_world::run_log::RunRecord;

use super::super::Failure;
use super::core::Core;

mod plan;
mod replay;
#[cfg(test)]
mod tests;

/// One resolved log plan: a live world stream or a capture replay.
pub(crate) struct LogStream {
    /// Live world's pod root; `None` replays capture files instead.
    pods_dir: Option<std::path::PathBuf>,
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
    /// The client asked for the last N rendered lines (`--last`, or `-f`'s default 20).
    tail: bool,
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

impl LogStream {
    #[allow(clippy::too_many_arguments)]
    fn live(
        pods_dir: std::path::PathBuf,
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
            tail: false,
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
            tail: false,
            catalog: None,
        }
    }

    /// Marks this plan as a `--last` request, so a followed stream announces caught-up history.
    fn with_tail(mut self) -> Self {
        self.tail = true;
        self
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
