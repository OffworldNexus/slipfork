//! `image.build` / `image.pull`: the daemon owns every native build, pull, and ref refresh.
//!
//! One process constructs the `BtrfsEngine`, `ImageLayout`, `OciPuller`, and `StepHost`, so the
//! step cache and `images/refs.json` have exactly one writer. Every build/pull event crosses the
//! socket as a typed [`ImageFrame`]; only the CLI renders it, so no colour or terminal decision
//! leaks into the daemon.

mod build;
mod pull;

use std::collections::BTreeMap;
use std::fs;
use std::io;
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use serde_json::{Value, json};
use slipfork_image::{
    Architecture, BuildEvent, ExternalImage, ImageLayout, ImageReference, OciPuller, StepHost,
    StepLog, Stream, external_references,
};
use slipfork_world::{BtrfsEngine, StoreLayout, WorldEngine};

use super::frames::write_frame;
use super::{Daemon, Failure, wire};
use crate::world::HostEnv;

/// One typed build/pull event; the CLI rebuilds a `BuildEvent` and renders every row itself.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(tag = "event", rename_all = "lowercase")]
pub(crate) enum ImageFrame {
    /// One instruction's lifecycle transition.
    Step {
        index: usize,
        total: usize,
        instruction: String,
        args: String,
        state: String,
        elapsed_ms: u64,
        #[serde(skip_serializing_if = "Option::is_none")]
        exit_code: Option<i32>,
        /// Typed tail phase (`sealing` …), rendered by the CLI as the row's suffix.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        phase: Option<String>,
        /// Transport-only alignment hint, omitted from every rendered `--json` object.
        #[serde(default)]
        width: usize,
    },
    /// One child-output line, tagged with the stream that produced it.
    Log { stream: String, line: String },
    /// One engine narration line.
    Note { text: String },
}

impl ImageFrame {
    /// Converts one typed engine event into its wire frame.
    pub(crate) fn from_event(event: BuildEvent<'_>) -> Self {
        match event {
            BuildEvent::Step(step) => Self::Step {
                index: step.index,
                total: step.total,
                instruction: step.instruction.to_owned(),
                args: step.args.to_owned(),
                state: step.state.as_str().to_owned(),
                elapsed_ms: u64::try_from(step.elapsed.as_millis()).unwrap_or(u64::MAX),
                exit_code: step.exit_code,
                phase: step
                    .phase
                    .map(|phase| crate::world::StepPin::tail_label(phase).to_owned()),
                width: step.instruction_width,
            },
            BuildEvent::Line { stream, text } => Self::Log {
                stream: match stream {
                    Stream::Stdout => "stdout".to_owned(),
                    Stream::Stderr => "stderr".to_owned(),
                },
                line: text.to_owned(),
            },
            BuildEvent::Note { text } => Self::Note {
                text: text.to_owned(),
            },
        }
    }

    /// The frame as the `event` payload the connection writes.
    pub(crate) fn value(&self) -> Value {
        serde_json::to_value(self).unwrap_or(Value::Null)
    }
}

impl Daemon {
    /// Builds or pulls one image under the daemon-wide image lock, returning its final summary.
    pub(super) fn dispatch_image(
        &self,
        params: &Value,
        emit: &mut dyn FnMut(Value) -> bool,
        build: bool,
    ) -> Result<Option<Value>, Failure> {
        let _guard = self
            .image_lock
            .lock()
            .map_err(|_| Failure::new("daemon-poisoned", "the daemon image lock is poisoned"))?;
        let host = {
            let core = self.core()?;
            core.host().clone()
        };
        if build {
            build::build(&host, params, emit)
        } else {
            pull::pull(&host, params, emit)
        }
    }

    /// Streams one `image.build`/`image.pull` request, then writes its final `ok` or `error`.
    pub(super) fn handle_image(
        &self,
        id: &str,
        method: &str,
        params: &Value,
        writer: &mut UnixStream,
    ) -> io::Result<()> {
        let mut closed = false;
        let build = method == "image.build";
        let outcome = {
            let mut emit = |event: Value| -> bool {
                if closed {
                    return false;
                }
                if write_frame(writer, &wire::event(id, event)).is_err() {
                    closed = true;
                    return false;
                }
                true
            };
            self.dispatch_image(params, &mut emit, build)
        };
        match outcome {
            Ok(Some(value)) if !closed => {
                write_frame(writer, &wire::ok(id, value))?;
            }
            Ok(_) => {}
            Err(failure) if failure.code == "client-closed" => {}
            Err(failure) => write_frame(writer, &wire::error(id, failure.code, &failure.message))?,
        }
        Ok(())
    }
}

/// The quiet refusal a build/pull returns once its client stopped reading.
pub(crate) fn client_closed() -> Failure {
    Failure::new("client-closed", "the client closed during the image stream")
}

/// The injected roots one daemon image operation needs.
pub(super) struct ImageLab {
    pub(super) store: StoreLayout,
    pub(super) engine: Arc<dyn WorldEngine>,
    pub(super) images: ImageLayout,
    pub(super) architecture: Architecture,
    agent: String,
}

impl ImageLab {
    /// Prepares this project's store and derives the host architecture.
    pub(super) fn for_project(host: &HostEnv, project: &Path) -> Result<Self, Failure> {
        let store = StoreLayout::at(host.store_root_for(project));
        let engine = BtrfsEngine::new(store.clone());
        engine.ensure_store().map_err(|error| {
            failure(
                "image-store",
                format!("cannot prepare the image store: {error}"),
                "check that this project's Btrfs store is writable and retry",
            )
        })?;
        let architecture = Architecture::from_oci(host.arch).ok_or_else(|| {
            failure(
                "image-store",
                format!("unsupported host architecture `{}`", host.arch),
                "run Slipfork on an amd64 or arm64 host",
            )
        })?;
        let images = ImageLayout::new(store.images_root());
        images.ensure().map_err(|error| {
            failure(
                "image-store",
                format!("cannot prepare the image cache: {error}"),
                "check that this project's Btrfs store is writable and retry",
            )
        })?;
        Ok(Self {
            store,
            engine: Arc::new(engine),
            images,
            architecture,
            agent: format!("slipfork/{}", env!("CARGO_PKG_VERSION")),
        })
    }

    pub(super) fn puller(&self, host: &HostEnv) -> OciPuller {
        OciPuller::new(
            Arc::clone(&self.engine),
            self.images.clone(),
            host.registry_credentials.clone(),
            self.architecture,
            self.agent.clone(),
        )
    }

    pub(super) fn step_host(&self, host: &HostEnv) -> Result<StepHost, Failure> {
        let crun = host.tool("crun").map_err(|message| {
            failure(
                "crun",
                message,
                "install crun and retry, or run `slipfork doctor`",
            )
        })?;
        Ok(StepHost::new(
            crun,
            host.resolv_conf.clone(),
            self.store.store_root().join("build"),
            host.uid,
            host.gid,
        ))
    }

    /// Removes one remembered reference before a pull refreshes it from the registry.
    pub(super) fn forget_ref(&self, reference: &str) -> Result<(), Failure> {
        forget_ref(&self.images, reference).map_err(|message| {
            failure(
                "image-index",
                message,
                "check that the image store is writable, then retry",
            )
        })
    }
}

/// The transport-level options every image request carries.
pub(super) struct ImageRequest {
    pub(super) project: PathBuf,
}

impl ImageRequest {
    pub(super) fn parse(params: &Value) -> Result<Self, Failure> {
        let project = params
            .get("project")
            .and_then(Value::as_str)
            .map(PathBuf::from)
            .ok_or_else(|| {
                failure(
                    "bad-params",
                    "`project` is required",
                    "send the absolute project directory",
                )
            })?;
        Ok(Self { project })
    }
}

/// The one typed failure whose message carries the exact `1 line + next:` remedy.
pub(super) fn failure(code: &'static str, message: impl Into<String>, next: &str) -> Failure {
    Failure::new(code, format!("{}\nnext: {next}", message.into()))
}

/// Removes `reference` from `images/refs.json` with one atomic rename; a missing file is a no-op.
pub(super) fn forget_ref(images: &ImageLayout, reference: &str) -> Result<(), String> {
    let path = images.refs();
    let mut entries: BTreeMap<String, String> = match fs::read(&path) {
        Ok(bytes) => serde_json::from_slice(&bytes).map_err(|error| {
            format!(
                "cannot read `{}`: {error}\nnext: fix or delete `{}`",
                path.display(),
                path.display()
            )
        })?,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => {
            return Err(format!(
                "cannot read `{}`: {error}\nnext: fix or delete `{}`",
                path.display(),
                path.display()
            ));
        }
    };
    if entries.remove(reference).is_none() {
        return Ok(());
    }
    let bytes = serde_json::to_vec_pretty(&entries)
        .map_err(|error| format!("cannot encode `{}`: {error}", path.display()))?;
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .map_err(|error| format!("cannot create `{}`: {error}", parent.display()))?;
    }
    let temporary = unique_temp(&path);
    fs::write(&temporary, bytes)
        .map_err(|error| format!("cannot write `{}`: {error}", temporary.display()))?;
    fs::rename(&temporary, &path)
        .map_err(|error| format!("cannot install `{}`: {error}", path.display()))
}

/// Records one successful build in `<images_root>/built.json` with an atomic rename.
pub(super) fn record_built(
    path: &Path,
    component: &str,
    target: Option<&String>,
    image_id: &str,
    digest: Option<&str>,
) -> Result<(), String> {
    let mut entries: BTreeMap<String, Value> = match fs::read(path) {
        Ok(bytes) => serde_json::from_slice(&bytes).map_err(|error| {
            format!(
                "cannot read `{}`: {error}\nnext: fix or delete `{}`",
                path.display(),
                path.display()
            )
        })?,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => BTreeMap::new(),
        Err(error) => {
            return Err(format!(
                "cannot read `{}`: {error}\nnext: fix or delete `{}`",
                path.display(),
                path.display()
            ));
        }
    };
    let mut entry = json!({ "image_id": image_id, "target": target });
    if let Some(digest) = digest {
        entry["digest"] = json!(digest);
    }
    entries.insert(component.to_owned(), entry);
    let bytes = serde_json::to_vec_pretty(&entries)
        .map_err(|error| format!("cannot encode `{}`: {error}", path.display()))?;
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .map_err(|error| format!("cannot create `{}`: {error}", parent.display()))?;
    }
    let temporary = unique_temp(path);
    fs::write(&temporary, bytes)
        .map_err(|error| format!("cannot write `{}`: {error}", temporary.display()))?;
    fs::rename(&temporary, path)
        .map_err(|error| format!("cannot install `{}`: {error}", path.display()))
}

/// Wall clock for snapshot records; the daemon is a composition root.
pub(super) fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |elapsed| {
            u64::try_from(elapsed.as_millis()).unwrap_or(u64::MAX)
        })
}

/// Pre-scans `source` and pulls every external `COPY --from=<image>` rootfs.
///
/// Each pull is one live `COPY` row at the instruction's own walked position and `Dockerfile`
/// line; a failure folds back onto that line instead of surfacing a step-less build error.
pub(crate) fn pull_externals(
    source: &str,
    target: Option<&str>,
    puller: &OciPuller,
    step_log: &mut StepLog<'_>,
) -> Result<BTreeMap<String, ExternalImage>, slipfork_image::Error> {
    let references = external_references(source, target)?;
    let mut externals = BTreeMap::new();
    for external in references {
        step_log.announce_at(
            external.place,
            "COPY",
            &format!("--from={}", external.reference),
        );
        let started = std::time::Instant::now();
        let outcome = match ImageReference::parse(&external.reference) {
            Ok(parsed) => puller.pull_into(&parsed, step_log),
            Err(error) => Err(error),
        };
        match outcome {
            Ok(image) => {
                step_log.done(started.elapsed());
                externals.insert(
                    external.reference.clone(),
                    ExternalImage {
                        rootfs: image.rootfs,
                        digest: image.digest,
                    },
                );
            }
            Err(error) => {
                step_log.failed(started.elapsed(), error.exit_code());
                return Err(external.pull_failure(&error));
            }
        }
    }
    Ok(externals)
}

/// A per-attempt sibling so two writers never share the fixed `.tmp` name.
fn unique_temp(path: &Path) -> PathBuf {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |elapsed| elapsed.as_nanos());
    path.with_extension(format!("json.{}-{nanos}.tmp", std::process::id()))
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::PathBuf;
    use std::time::{SystemTime, UNIX_EPOCH};

    use slipfork_image::ImageLayout;

    use super::forget_ref;

    /// The wire `phase` label is the one `StepPin` vocabulary, both directions.
    #[test]
    fn tail_phase_labels_round_trip_through_the_wire() {
        use slipfork_world::snapshot::TailPhase;

        assert_eq!(
            crate::world::StepPin::tail_phase("sealing"),
            Some(TailPhase::Sealing)
        );
        assert_eq!(
            crate::world::StepPin::tail_label(TailPhase::Sealing),
            "sealing"
        );
    }

    /// A forgotten reference is gone before the pull re-resolves, and the next remember wins.
    #[test]
    fn forgetting_a_ref_clears_it_and_the_next_remember_wins() {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("clock reads after the epoch")
            .as_nanos();
        let root = PathBuf::from("/var/tmp")
            .join(format!("slipfork-forget-{}-{nanos}", std::process::id()));
        fs::create_dir_all(&root).expect("create the real refs lab root");
        let layout = ImageLayout::new(&root);
        layout.ensure().expect("create the OCI cache root");
        let reference = "127.0.0.1:5000/app:latest";
        let old = "sha256:1111111111111111111111111111111111111111111111111111111111111111";
        let fresh = "sha256:2222222222222222222222222222222222222222222222222222222222222222";
        layout.remember_ref(reference, old);
        assert_eq!(layout.cached_ref(reference).as_deref(), Some(old));

        forget_ref(&layout, reference).expect("forget the reference atomically");
        assert_eq!(layout.cached_ref(reference), None, "the old tag is gone");
        let stored = fs::read_to_string(layout.refs()).expect("the rewritten refs file");
        assert!(!stored.contains(old), "{stored}");

        layout.remember_ref(reference, fresh);
        assert_eq!(layout.cached_ref(reference).as_deref(), Some(fresh));
        let _ = fs::remove_dir_all(&root);
    }
}
