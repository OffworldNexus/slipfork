//! `docker pull` and `docker build`: thin clients of the daemon's typed image streams.
//!
//! The daemon owns the one OCI puller and native builder, the step cache, and the credentials;
//! this module connects, sends `image.pull`/`image.build`, decodes each typed frame, and renders
//! it through the CLI's [`EventPrinter`]. A reader that closes stdout (`| head`) ends quietly.

use std::cell::Cell;
use std::io::Write;
use std::os::fd::RawFd;
use std::process::ExitCode;
use std::rc::Rc;

use serde_json::{Value, json};

use crate::commands::Session;
use crate::daemon::DaemonClient;
use crate::daemon::image::ImageFrame;
use crate::ui::Palette;

use super::printer::EventPrinter;
use super::{DockerArgs, DockerVerb};

/// Which final row a streamed image call prints after its last frame.
#[derive(Debug, Clone, Copy)]
enum FinalVerb {
    Build,
    Pull,
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

    /// Sends one image request, renders every typed frame, and prints the final summary.
    fn relay_image(
        &self,
        method: &str,
        params: Value,
        args: &DockerArgs,
        verb: FinalVerb,
        next: &str,
    ) -> ExitCode {
        let mut client = match DaemonClient::connect(self.host()) {
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
        let palette = Palette::new(self.color() && !args.json);
        let broken = Rc::new(Cell::new(false));
        let stdout = std::io::stdout();
        let mut stdout = stdout.lock();
        let outcome = {
            let flag = Rc::clone(&broken);
            let mut sink = |chunk: &str| {
                if flag.get() {
                    return;
                }
                if stdout
                    .write_all(chunk.as_bytes())
                    .and_then(|()| stdout.flush())
                    .is_err()
                {
                    flag.set(true);
                    // The reader stopped (`| head`): drop the socket so the daemon stops too.
                    // SAFETY: `descriptor` stays open for the whole call.
                    unsafe {
                        libc::shutdown(descriptor, libc::SHUT_RDWR);
                    }
                }
            };
            let notes = matches!(verb, FinalVerb::Pull);
            let mut printer = EventPrinter::new(
                palette,
                self.interactive() && !args.json,
                args.verbose,
                args.json,
                notes,
                &mut sink,
            );
            let flag = Rc::clone(&broken);
            let outcome = client.call_streaming(method, params, &mut |event| {
                if flag.get() {
                    return;
                }
                if let Ok(frame) = serde_json::from_value::<ImageFrame>(event.clone()) {
                    printer.frame(&frame);
                }
            });
            if let Ok(reply) = &outcome
                && !broken.get()
            {
                final_row(reply, &mut printer, palette, args.json, verb);
            }
            outcome
        };
        if broken.get() {
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

/// Renders the daemon's final summary object, unchanged from the pre-daemon CLI bytes.
fn final_row(
    reply: &Value,
    printer: &mut EventPrinter<'_>,
    palette: Palette,
    json: bool,
    verb: FinalVerb,
) {
    if json {
        let mut value = reply.clone();
        if let Some(object) = value.as_object_mut() {
            object.remove("exit");
        }
        printer.chunk(&format!("{value}\n"));
        return;
    }
    match verb {
        FinalVerb::Pull => {
            let reference = reply["reference"].as_str().unwrap_or_default();
            let digest = reply["digest"].as_str().unwrap_or_default();
            let state = if reply["cached"].as_bool() == Some(true) {
                "cached"
            } else {
                "sealed"
            };
            printer.chunk(&format!(
                "{} Pulled {} {} {}\n",
                palette.icon_ok(),
                palette.value(reference),
                palette.meta(digest),
                palette.meta(state)
            ));
        }
        FinalVerb::Build => {
            let component = reply["component"].as_str().unwrap_or_default();
            let image_id = reply["image_id"].as_str().unwrap_or_default();
            let hits = reply["cache_hits"].as_u64().unwrap_or(0);
            let misses = reply["cache_misses"].as_u64().unwrap_or(0);
            printer.chunk(&format!(
                "{} Built {} {} {}\n",
                palette.icon_ok(),
                palette.tint(component, "1"),
                palette.meta(image_id),
                palette.meta(&format!("({hits} hits, {misses} misses)"))
            ));
        }
    }
}
