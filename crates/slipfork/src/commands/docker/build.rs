//! `docker pull` and `docker build`: thin clients of the daemon's typed image streams.
//!
//! The daemon owns the one OCI puller and native builder, the step cache, and the credentials;
//! this module connects, sends `image.pull`/`image.build`, decodes each typed frame, and streams
//! it through [`ReportRenderer`] rows. A reader that closes stdout (`| head`) ends quietly.

use std::io::{self, Write};
use std::os::fd::RawFd;
use std::process::ExitCode;

use serde_json::{Value, json};

use crate::commands::Session;
use crate::daemon::image::ImageFrame;
use crate::ui::report::{
    Counter, Done, Entry, Outcome, ReportRenderer, Row, RowLevel, StreamKind, digest_cell,
    visible_line,
};

use super::{DockerArgs, DockerVerb};

/// Which final row a streamed image call prints after its last frame.
#[derive(Debug, Clone, Copy)]
enum FinalVerb {
    Build,
    Pull,
}

/// One image stream: typed frames in, report rows or NDJSON out, broken-pipe state in between.
struct ImageStream<'a> {
    renderer: ReportRenderer,
    json: bool,
    verbose: bool,
    notes: bool,
    verb: FinalVerb,
    out: &'a mut dyn Write,
    broken: bool,
    failed: bool,
    /// The live socket, shut down once the reader stops; `None` in buffer tests.
    descriptor: Option<RawFd>,
}

impl<'a> ImageStream<'a> {
    fn new(
        renderer: ReportRenderer,
        json: bool,
        verbose: bool,
        verb: FinalVerb,
        out: &'a mut dyn Write,
        descriptor: Option<RawFd>,
    ) -> Self {
        Self {
            renderer,
            json,
            verbose,
            notes: matches!(verb, FinalVerb::Pull),
            verb,
            out,
            broken: false,
            failed: false,
            descriptor,
        }
    }

    fn should_stop(&self) -> bool {
        self.broken || self.failed
    }

    const fn broken(&self) -> bool {
        self.broken
    }

    const fn failed(&self) -> bool {
        self.failed
    }

    /// Renders one typed image frame.
    fn frame(&mut self, frame: &ImageFrame) {
        match frame {
            ImageFrame::Step {
                index,
                total,
                instruction,
                args,
                state,
                elapsed_ms,
                exit_code,
                phase: _,
                width,
            } => {
                // `--json` forwards every transition, including `started`.
                if self.json {
                    let mut value = json!({
                        "event": "step",
                        "index": index,
                        "total": total,
                        "instruction": instruction,
                        "args": args,
                        "state": state,
                        "elapsed_ms": elapsed_ms,
                    });
                    if let Some(code) = exit_code {
                        value["exit_code"] = json!(code);
                    }
                    self.write_json(&value);
                    return;
                }
                if state == "tail" {
                    return;
                }
                if state == "started" && !self.renderer.mode().interactive() {
                    return;
                }
                let row = step_row(StepFacts {
                    index: *index,
                    total: *total,
                    instruction,
                    args,
                    state,
                    elapsed_ms: *elapsed_ms,
                    exit_code: *exit_code,
                    width: *width,
                });
                self.write_entry(&Entry::Row(row));
            }
            ImageFrame::Log { stream, line } => {
                let text = visible_line(line, self.renderer.color(), self.json);
                if text.trim().is_empty() {
                    return;
                }
                if self.json {
                    self.write_json(
                        &json!({ "event": "log", "stream": stream, "line": text.as_ref() }),
                    );
                } else {
                    let stream = match stream.as_str() {
                        "stderr" => StreamKind::Stderr,
                        _ => StreamKind::Stdout,
                    };
                    self.write_entry(&Entry::Stream {
                        indent: 2,
                        stream,
                        line: text.into_owned(),
                        ts_ms: 0,
                    });
                }
            }
            ImageFrame::Note { text } => {
                if self.json {
                    self.write_json(&json!({ "event": "note", "text": text }));
                } else if self.notes || self.verbose {
                    self.write_note(2, text);
                }
            }
        }
    }

    /// Renders the daemon's final summary object, unchanged from the pre-daemon CLI bytes.
    fn final_row(&mut self, reply: &Value) {
        if self.json {
            let mut value = reply.clone();
            if let Some(object) = value.as_object_mut() {
                object.remove("exit");
            }
            self.write_json(&value);
            return;
        }
        let palette = self.renderer.palette();
        let line = match self.verb {
            FinalVerb::Pull => {
                let reference = reply["reference"].as_str().unwrap_or_default();
                let digest = reply["digest"].as_str().unwrap_or_default();
                let state = if reply["cached"].as_bool() == Some(true) {
                    "cached"
                } else {
                    "sealed"
                };
                format!(
                    "{} Pulled {} {} {}\n",
                    palette.icon_ok(),
                    palette.value(reference),
                    palette.meta(&digest_cell(digest)),
                    palette.meta(state)
                )
            }
            FinalVerb::Build => {
                let component = reply["component"].as_str().unwrap_or_default();
                let image_id = reply["image_id"].as_str().unwrap_or_default();
                let hits = reply["cache_hits"].as_u64().unwrap_or(0);
                let misses = reply["cache_misses"].as_u64().unwrap_or(0);
                format!(
                    "{} Built {} {} {}\n",
                    palette.icon_ok(),
                    palette.value(component),
                    palette.meta(&digest_cell(image_id)),
                    palette.meta(&format!("({hits} hits, {misses} misses)"))
                )
            }
        };
        self.write_text(&line);
    }

    fn write_entry(&mut self, entry: &Entry) {
        let mut err = io::sink();
        let result = self.renderer.emit_entry_to(entry, self.out, &mut err);
        self.write_result(result);
    }

    fn write_note(&mut self, indent: usize, text: &str) {
        let result = self.renderer.write_note(indent, text, self.out);
        self.write_result(result);
    }

    fn write_json(&mut self, value: &impl serde::Serialize) {
        let result = serde_json::to_writer(&mut *self.out, value)
            .map_err(io::Error::other)
            .and_then(|()| self.out.write_all(b"\n"));
        self.write_result(result);
    }

    fn write_text(&mut self, text: &str) {
        let result = self.out.write_all(text.as_bytes());
        self.write_result(result);
    }

    /// Remembers one write failure and drops the live socket, so the daemon stops too.
    fn write_result(&mut self, result: io::Result<()>) {
        if let Err(error) = result {
            if error.kind() != io::ErrorKind::BrokenPipe {
                self.failed = true;
            }
            self.broken = true;
            if let Some(descriptor) = self.descriptor {
                // SAFETY: the descriptor stays open for the whole call.
                unsafe {
                    libc::shutdown(descriptor, libc::SHUT_RDWR);
                }
            }
        }
    }
}

impl Session {
    /// Refreshes one OCI reference through the daemon and renders its `Pulled` row.
    pub(super) fn docker_pull(&self, args: &DockerArgs) -> ExitCode {
        let DockerVerb::Pull { reference } = &args.verb else {
            unreachable!("docker_pull is only reachable through DockerVerb::Pull")
        };
        self.relay_image(
            "image.pull",
            json!({ "project": self.cwd(), "reference": reference }),
            args,
            FinalVerb::Pull,
            "check the reference and registry credentials, then retry",
        )
    }

    /// Builds one `Forkfile` image component through the daemon, rendering every step row.
    pub(super) fn docker_build(&self, args: &DockerArgs) -> ExitCode {
        let DockerVerb::Build {
            component,
            target,
            no_cache,
        } = &args.verb
        else {
            unreachable!("docker_build is only reachable through DockerVerb::Build")
        };
        self.relay_image(
            "image.build",
            json!({
                "project": self.cwd(),
                "component": component,
                "target": target,
                "no_cache": no_cache,
            }),
            args,
            FinalVerb::Build,
            "fix the failing Dockerfile instruction and retry; run `slipfork logs --runs` to find the run id, then replay it",
        )
    }

    /// Sends one image request, streams every typed frame, and prints the final summary.
    fn relay_image(
        &self,
        method: &str,
        params: Value,
        args: &DockerArgs,
        verb: FinalVerb,
        next: &str,
    ) -> ExitCode {
        let mut client = match self.connect() {
            Ok(client) => client,
            Err(error) => {
                return self.report(
                    &error.code,
                    &error.message,
                    "start the daemon with `slipfork up` and retry",
                    args.json,
                );
            }
        };
        let descriptor: RawFd = client.stream_fd();
        let renderer = self.renderer().with_verbose(args.verbose);
        let (outcome, broken, failed) = {
            let stdout = io::stdout();
            let mut out = stdout.lock();
            let mut stream = ImageStream::new(
                renderer,
                args.json,
                args.verbose,
                verb,
                &mut out,
                Some(descriptor),
            );
            let outcome = client.call_streaming(method, params, &mut |event| {
                if stream.should_stop() {
                    return;
                }
                let Ok(frame) = serde_json::from_value::<ImageFrame>(event.clone()) else {
                    return;
                };
                stream.frame(&frame);
            });
            if let Ok(reply) = &outcome
                && !stream.should_stop()
            {
                stream.final_row(reply);
            }
            (outcome, stream.broken(), stream.failed())
        };
        if broken {
            if failed {
                return self.report(
                    "stdout",
                    "cannot write the image stream to stdout",
                    "check that stdout is writable and retry",
                    args.json,
                );
            }
            return ExitCode::SUCCESS;
        }
        match outcome {
            Ok(reply) => {
                let code = reply.get("exit").and_then(Value::as_i64).unwrap_or(0);
                ExitCode::from(u8::try_from(code.clamp(0, 255)).unwrap_or(1))
            }
            Err(error) => self.report(&error.code, &error.message, next, args.json),
        }
    }
}

/// One Dockerfile instruction row with its plan width latched once.
struct StepFacts<'a> {
    index: usize,
    total: usize,
    instruction: &'a str,
    args: &'a str,
    state: &'a str,
    elapsed_ms: u64,
    exit_code: Option<i32>,
    width: usize,
}

/// One Dockerfile instruction row with its plan width latched once.
fn step_row(facts: StepFacts<'_>) -> Row {
    let StepFacts {
        index,
        total,
        instruction,
        args,
        state,
        elapsed_ms,
        exit_code,
        width,
    } = facts;
    let outcome = match state {
        "started" => Outcome::Running,
        "failed" => Outcome::Failed { at: None },
        _ => Outcome::Ok(Done::Step),
    };
    Row {
        level: RowLevel::Step {
            instruction_width: width,
        },
        counter: Some(Counter { index, total }),
        keyword: Some(instruction.to_owned()),
        target: (!args.is_empty()).then(|| args.to_owned()),
        state: outcome,
        command: exit_code.map(|code| format!("(exit {code})")),
        timing_ms: (state != "started").then_some(elapsed_ms),
        id: None,
    }
}

#[cfg(test)]
mod tests;
