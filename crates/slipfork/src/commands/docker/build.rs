//! `docker pull` and `docker build`: one live event stream over the existing image engine.
//!
//! The puller and builder are the exact `OciPuller`/`NativeImageBuilder` that `up` constructs, so
//! a CLI pull warms the same sealed cache and a CLI build shares the same step keys. Nothing here
//! boots a pod or contacts the daemon.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use serde_json::{Value, json};
use slipfork_forkfile::compile;
use slipfork_image::{
    Architecture, BuildArgs, BuildEvent, BuildRequest, Cache, ImageLayout, ImageReference,
    NativeImageBuilder, OciPuller, StepEvent, StepHost, StepLog, StepState, Stream,
    planned_instruction_width,
};
use slipfork_world::{BtrfsEngine, StoreLayout, WorldEngine};

use crate::commands::{Session, emit};
use crate::ui::{Palette, UiMode, elapsed_text};
use crate::world::features::WorldFeatures;

use super::{DockerArgs, DockerVerb};

/// The injected roots one CLI image operation needs.
struct ImageLab {
    store: StoreLayout,
    engine: Arc<dyn WorldEngine>,
    architecture: Architecture,
    agent: String,
}

impl ImageLab {
    /// Prepares this directory's project store and derives the host architecture.
    fn for_session(session: &Session) -> Result<Self, String> {
        let store = StoreLayout::at(session.host().store_root_for(session.cwd()));
        let engine = BtrfsEngine::new(store.clone());
        engine
            .ensure_store()
            .map_err(|error| format!("cannot prepare the image store: {error}"))?;
        let architecture = Architecture::from_oci(session.host().arch)
            .ok_or_else(|| format!("unsupported host architecture `{}`", session.host().arch))?;
        Ok(Self {
            store,
            engine: Arc::new(engine),
            architecture,
            agent: format!("slipfork/{}", env!("CARGO_PKG_VERSION")),
        })
    }

    fn puller(&self, session: &Session) -> OciPuller {
        OciPuller::new(
            Arc::clone(&self.engine),
            ImageLayout::new(self.store.images_root()),
            session.host().registry_credentials.clone(),
            self.architecture,
            self.agent.clone(),
        )
    }

    fn step_host(&self, session: &Session) -> Result<StepHost, String> {
        let crun = session.host().tool("crun")?;
        Ok(StepHost::new(
            crun,
            session.host().resolv_conf.clone(),
            self.store.store_root().join("build"),
            session.host().uid,
            session.host().gid,
        ))
    }
}

impl Session {
    /// Pulls one OCI reference, streaming the engine's events live.
    pub(super) fn docker_pull(&self, args: &DockerArgs) -> ExitCode {
        let DockerVerb::Pull { reference } = &args.verb else {
            unreachable!("docker_pull is only reachable through DockerVerb::Pull")
        };
        let parsed = match ImageReference::parse(reference) {
            Ok(parsed) => parsed,
            Err(error) => {
                return self.report(
                    "invalid-reference",
                    &format!("invalid image reference `{reference}`: {}", error.cause()),
                    "use a full reference such as `ghcr.io/owner/image:tag`",
                    args.json,
                );
            }
        };
        let lab = match ImageLab::for_session(self) {
            Ok(lab) => lab,
            Err(message) => {
                return self.report(
                    "image-store",
                    &message,
                    "check that this project's Btrfs store is writable and retry",
                    args.json,
                );
            }
        };
        let puller = lab.puller(self);
        let pulled = {
            let mut printer = EventPrinter::new(self.ui(), args.verbose, true);
            let mut bus = |event: BuildEvent<'_>| printer.event(event);
            puller.pull_row(&parsed, reference, &mut StepLog::with_events(&mut bus))
        };
        let pulled = match pulled {
            Ok(pulled) => pulled,
            Err(error) => {
                return self.report(
                    "registry-pull",
                    &format!("pull `{parsed}` failed: {}", error.cause()),
                    "check the reference and registry credentials, then retry",
                    args.json,
                );
            }
        };
        let digest = digest_label(&pulled.digest);
        if args.json {
            return emit(&format!(
                "{}\n",
                json!({
                    "reference": parsed.to_string(),
                    "digest": digest,
                    "rootfs": pulled.rootfs.display().to_string(),
                    "cached": pulled.cached,
                })
            ));
        }
        let palette = Palette::new(self.color());
        emit(&format!(
            "{} Pulled {} {} {}\n",
            palette.icon_ok(),
            palette.value(&parsed.to_string()),
            palette.meta(&digest),
            palette.meta(if pulled.cached { "cached" } else { "sealed" })
        ))
    }

    /// Builds one `Forkfile` image component, streaming every Dockerfile instruction live.
    pub(super) fn docker_build(&self, args: &DockerArgs) -> ExitCode {
        let DockerVerb::Build {
            component,
            target,
            no_cache,
        } = &args.verb
        else {
            unreachable!("docker_build is only reachable through DockerVerb::Build")
        };
        let path = self.cwd().join("Forkfile");
        let source = match fs::read_to_string(&path) {
            Ok(source) => source,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                return self.report(
                    "no-forkfile",
                    &format!("no Forkfile in `{}`", self.cwd().display()),
                    "create a Forkfile with an `image` block, then retry",
                    args.json,
                );
            }
            Err(error) => {
                return self.report(
                    "forkfile",
                    &format!("cannot read `{}`: {error}", path.display()),
                    "check the Forkfile permissions and retry",
                    args.json,
                );
            }
        };
        let checked = match compile(&source) {
            Ok(checked) => checked,
            Err(diagnostics) => {
                let diagnostic = diagnostics.first();
                let message = diagnostic.map_or_else(
                    || "the Forkfile is invalid".to_owned(),
                    |diagnostic| {
                        format!(
                            "Forkfile:{}: {}",
                            diagnostic.span.line(),
                            diagnostic.message
                        )
                    },
                );
                let next = diagnostic.map_or_else(
                    || "run `slipfork fmt` after fixing the first error".to_owned(),
                    |diagnostic| diagnostic.next.clone(),
                );
                return self.report("invalid-forkfile", &message, &next, args.json);
            }
        };
        let doc = &checked.document;
        let features = match WorldFeatures::load_resolved(self.cwd(), doc) {
            Ok(features) => features,
            Err(message) => {
                return self.report("feature", &message, WorldFeatures::REMEDY, args.json);
            }
        };
        let plan = match slipfork_world::world_plan_with(doc, self.cwd(), &features) {
            Ok(plan) => plan,
            Err(error) => {
                return self.report(
                    "forkfile-plan",
                    &error.to_string(),
                    "fix the Forkfile, then run `slipfork doctor`",
                    args.json,
                );
            }
        };
        let Some(node) = plan.images.iter().find(|node| node.id == *component) else {
            return self.report(
                "unknown-component",
                &format!("no image component `{component}` in this Forkfile"),
                "run `slipfork status` to see this world's declared images",
                args.json,
            );
        };
        if let Some(from) = &node.from {
            return self.report(
                "not-buildable",
                &format!("image `{component}` is pulled from `{from}`, not built"),
                &format!("run `slipfork docker pull {from}` instead"),
                args.json,
            );
        }

        let context = self.cwd().join(&node.directory);
        let dockerfile = context.join(
            node.dockerfile
                .clone()
                .unwrap_or_else(|| PathBuf::from("Dockerfile")),
        );
        let target = target
            .clone()
            .or_else(|| doc.eval_image_stage_with(component, &features));
        let world = crate::world::world_name(self.cwd(), None)
            .map_or_else(|_| "slipfork".to_owned(), |world| world.as_str().to_owned());
        let mut build_args = BuildArgs::new();
        for (key, value) in doc.eval_image_args_with(&world, "http", component, &features) {
            build_args.declare(key, value);
        }
        let request = BuildRequest {
            context,
            dockerfile,
            args: build_args,
            target,
            externals: BTreeMap::new(),
        };
        let lab = match ImageLab::for_session(self) {
            Ok(lab) => lab,
            Err(message) => {
                return self.report(
                    "image-store",
                    &message,
                    "check that this project's Btrfs store is writable and retry",
                    args.json,
                );
            }
        };
        let step_host = match lab.step_host(self) {
            Ok(host) => host,
            Err(message) => {
                return self.report(
                    "crun",
                    &message,
                    "install crun and retry, or run `slipfork doctor`",
                    args.json,
                );
            }
        };
        let source = match fs::read_to_string(&request.dockerfile) {
            Ok(source) => source,
            Err(error) => {
                return self.report(
                    "dockerfile",
                    &format!("cannot read `{}`: {error}", request.dockerfile.display()),
                    "create the Dockerfile named by this image, then retry",
                    args.json,
                );
            }
        };
        let planned_width =
            planned_instruction_width(&source, request.target.as_deref()).unwrap_or(0);
        let puller = lab.puller(self);
        let mut printer = EventPrinter::new(self.ui(), args.verbose, false);
        let outcome = {
            let mut bus = |event: BuildEvent<'_>| printer.event(event);
            let mut log = StepLog::with_events(&mut bus);
            log.plan_width(planned_width);
            let pulled = match crate::world::pull_externals(
                &source,
                request.target.as_deref(),
                &puller,
                &mut log,
            ) {
                Ok(externals) => externals,
                Err(error) => {
                    return self.report(
                        "image-build",
                        &format!("pull external image failed: {}", error.cause()),
                        "check the `COPY --from` reference and registry credentials, then retry",
                        args.json,
                    );
                }
            };
            let build_puller = lab.puller(self).sharing_flights(&puller);
            let builder = NativeImageBuilder::new(
                Arc::clone(&lab.engine),
                build_puller,
                lab.store.clone(),
                lab.architecture,
                step_host,
            )
            .with_cache(if *no_cache { Cache::Cold } else { Cache::Warm });
            builder.build(
                &BuildRequest {
                    externals: pulled,
                    ..request.clone()
                },
                &mut log,
                now_ms(),
            )
        };
        let report = match outcome {
            Ok(report) => report,
            Err(error) => {
                return self.report(
                    "image-build",
                    &format!("build `{component}` failed: {}", error.cause()),
                    "fix the failing Dockerfile instruction and retry, or run `slipfork logs` after `up`",
                    args.json,
                );
            }
        };
        let cache_key = report.key;
        let image_id = format!("sha256:{cache_key}");
        let digest = report.base_digest;
        let index = lab.store.images_root().join("built.json");
        if let Err(error) = record_built(
            &index,
            component,
            request.target.as_ref(),
            &image_id,
            digest.as_deref(),
        ) {
            return self.report(
                "image-index",
                &format!("cannot record the built image: {error}"),
                "check that the image store is writable, then retry",
                args.json,
            );
        }
        if args.json {
            let mut value = json!({
                "component": component,
                "target": request.target,
                "image_id": image_id,
                "cache_key": cache_key,
                "rootfs": report.rootfs.display().to_string(),
                "cache_hits": report.cache_hits,
                "cache_misses": report.cache_misses,
                "no_cache": no_cache,
            });
            if let Some(digest) = &digest {
                value["digest"] = json!(digest);
            }
            return emit(&format!("{value}\n"));
        }
        let palette = Palette::new(self.color());
        emit(&format!(
            "{} Built {} {} {}\n",
            palette.icon_ok(),
            palette.tint(component, "1"),
            palette.meta(&image_id),
            palette.meta(&format!(
                "({} hits, {} misses)",
                report.cache_hits, report.cache_misses
            ))
        ))
    }
}

/// Renders one build event: live human rows, or NDJSON frames under `--json`.
struct EventPrinter {
    mode: UiMode,
    verbose: bool,
    notes: bool,
    palette: Palette,
}

impl EventPrinter {
    fn new(mode: UiMode, verbose: bool, notes: bool) -> Self {
        Self {
            mode,
            verbose,
            notes,
            palette: Palette::new(mode.color()),
        }
    }

    fn event(&mut self, event: BuildEvent<'_>) {
        match event {
            BuildEvent::Step(step) => self.step(step),
            BuildEvent::Line { stream, text } => {
                let text = strip_ansi(text).replace('\r', "");
                if text.trim().is_empty() {
                    return;
                }
                let stream = stream_name(stream);
                if self.mode.json() {
                    out(&format!(
                        "{}\n",
                        json!({ "event": "log", "stream": stream, "line": text })
                    ));
                } else {
                    out(&format!("  {} {}\n", self.palette.gutter(stream), text));
                }
            }
            BuildEvent::Note { text } => {
                if self.mode.json() {
                    out(&format!("{}\n", json!({ "event": "note", "text": text })));
                } else if self.notes || self.verbose {
                    out(&format!("  {}\n", self.palette.meta(text)));
                }
            }
        }
    }

    fn step(&mut self, step: StepEvent<'_>) {
        if self.mode.json() {
            let mut frame = json!({
                "event": "step",
                "index": step.index,
                "total": step.total,
                "instruction": step.instruction,
                "args": step.args,
                "state": step.state.as_str(),
                "elapsed_ms": u64::try_from(step.elapsed.as_millis()).unwrap_or(u64::MAX),
            });
            if let Some(code) = step.exit_code {
                frame["exit_code"] = json!(code);
            }
            out(&format!("{frame}\n"));
            return;
        }
        let header = format!("[{}/{}]", step.index, step.total);
        let keyword = keyword_cell(self.palette, &step);
        let args = if step.args.is_empty() {
            String::new()
        } else {
            format!(" {}", self.palette.value(step.args))
        };
        match step.state {
            StepState::Started => {
                if self.mode.interactive() {
                    out(&format!(
                        "{} {} {}{}\n",
                        self.palette.tint("→", "36"),
                        self.palette.meta(&header),
                        keyword,
                        args
                    ));
                }
            }
            StepState::Done | StepState::Failed => {
                let icon = if step.state == StepState::Done {
                    self.palette.icon_ok()
                } else {
                    self.palette.icon_bad()
                };
                let elapsed =
                    elapsed_text(u64::try_from(step.elapsed.as_millis()).unwrap_or(u64::MAX));
                let exit = step
                    .exit_code
                    .map_or_else(String::new, |code| format!(" (exit {code})"));
                out(&format!(
                    "{} {} {}{} {}{}\n",
                    icon,
                    self.palette.meta(&header),
                    keyword,
                    args,
                    self.palette.meta(&elapsed),
                    exit
                ));
            }
        }
    }
}

/// The lowercase instruction keyword, in the substep role, padded outside the tint.
fn keyword_cell(palette: Palette, step: &StepEvent<'_>) -> String {
    let keyword = step.instruction.to_lowercase();
    let pad = step
        .instruction_width
        .saturating_sub(keyword.chars().count());
    format!("{}{}", palette.substep(&keyword), " ".repeat(pad))
}

fn stream_name(stream: Stream) -> &'static str {
    match stream {
        Stream::Stdout => "stdout",
        Stream::Stderr => "stderr",
    }
}

fn digest_label(value: &str) -> String {
    if value.starts_with("sha256:") {
        value.to_owned()
    } else {
        format!("sha256:{value}")
    }
}

/// Strips CSI escape sequences from child output, keeping the `\r` policy at the call site.
fn strip_ansi(text: &str) -> std::borrow::Cow<'_, str> {
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

/// Records one successful build in `<images_root>/built.json` with an atomic rename.
fn record_built(
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
    let temporary = path.with_extension("json.tmp");
    fs::write(&temporary, bytes)
        .map_err(|error| format!("cannot write `{}`: {error}", temporary.display()))?;
    fs::rename(&temporary, path)
        .map_err(|error| format!("cannot install `{}`: {error}", path.display()))
}

fn out(text: &str) {
    let _ = emit(text);
}

/// Wall clock for snapshot records; this CLI command is a composition root.
fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |elapsed| {
            u64::try_from(elapsed.as_millis()).unwrap_or(u64::MAX)
        })
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use slipfork_image::{StepEvent, StepPlace, StepState};

    use super::{Palette, keyword_cell};

    fn cell(instruction: &'static str, width: usize, color: bool) -> String {
        let mut event = StepEvent::new(
            StepPlace::new(1, 1, 1),
            instruction,
            "",
            StepState::Done,
            Duration::ZERO,
            None,
        );
        event.instruction_width = width;
        keyword_cell(Palette::new(color), &event)
    }

    #[test]
    fn keyword_cell_pads_outside_the_tint_to_the_plan_width() {
        for instruction in ["FROM", "COPY", "RUN", "WORKDIR"] {
            let plain = cell(instruction, 7, false);
            let keyword = instruction.to_lowercase();
            assert_eq!(plain.len(), 7, "{plain:?}");
            assert_eq!(plain, format!("{keyword}{}", " ".repeat(7 - keyword.len())));
            assert!(!plain.contains('\u{1b}'), "plain cells are unstyled");
        }

        assert_eq!(cell("FROM", 7, true), "\u{1b}[38;5;132mfrom\u{1b}[0m   ");
        assert_eq!(cell("COPY", 7, true), "\u{1b}[38;5;132mcopy\u{1b}[0m   ");
        assert_eq!(cell("RUN", 7, true), "\u{1b}[38;5;132mrun\u{1b}[0m    ");
        assert_eq!(cell("WORKDIR", 7, true), "\u{1b}[38;5;132mworkdir\u{1b}[0m");
        let padded = cell("FROM", 7, true);
        assert!(
            padded.ends_with("\u{1b}[0m   "),
            "padding is outside the tint: {padded:?}"
        );
    }
}
