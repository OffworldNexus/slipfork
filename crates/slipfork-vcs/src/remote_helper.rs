//! Pure `git-remote-slipfork` remote-helper line protocol over `BufRead` / `Write`.
//!
//! The crate ships no binary: `crates/slipfork`'s `git-remote-slipfork` executable delegates here,
//! holding the single Composition Root (process wiring, daemon socket, `PATH`). This module only
//! speaks Git's documented `gitremote-helpers` protocol (`capabilities`, `list`, `fetch`, `push`,
//! `option`) against an injected [`VcsWorkspace`], which is why it owns zero filesystem, process,
//! clock, or environment access.
//!
//! Ref frames use the documented `<sha> <refname>` single-space layout (one line per fork ref,
//! terminated by a blank line). `list` advertises `refs/heads/<fork>` and
//! `refs/slipfork/<fork>/wip` — never `refs/heads/<fork>/wip`, which Git's D/F refname lock forbids
//! (`ADR-0005` §3) — so both `git pull slipfork <fork>` and
//! `git fetch slipfork '+refs/slipfork/<fork>/wip:refs/slipfork/<fork>/wip'` resolve.
//!
//! A fork world lives inside the caller's own repository, so the commits `list` advertises are
//! already in the object database Git fetches into: `fetch` verifies every requested
//! `<sha> <refname>` pair against [`VcsWorkspace::resolve`] and answers with the batch-terminating
//! blank line instead of streaming an empty pack. A request for anything the workspace did not
//! advertise fails closed.

use std::fmt::Write as _;
use std::io::{BufRead, Read, Write};

use thiserror::Error;

use crate::ports::{VcsError, VcsWorkspace, WorkspaceRefs};

/// Hard cap on one protocol frame (terminating newline included).
///
/// A frame is a single command line, so anything larger is a protocol violation rather than a
/// legitimate request; the cap keeps a hostile or confused caller from growing an unbounded buffer.
pub const MAX_FRAME_BYTES: usize = 64 * 1024;

/// Capabilities advertised in response to `capabilities`, in order.
const CAPABILITIES: [&str; 3] = ["fetch", "push", "option"];

/// Options this helper honours (it prints no progress and no chatter, so both are vacuous).
const SUPPORTED_OPTIONS: [&str; 2] = ["progress", "verbosity"];

/// Reason reported for every refused `push`.
///
/// Worlds are fetched, not pushed over the remote helper in `0.2.0`; the developer commits inside
/// the world's checkout and the fork refs follow.
const PUSH_REFUSAL: &str =
    "fork worlds are fetched, not pushed: commit inside the world and re-fetch its fork refs";

/// Errors produced while serving one remote-helper session.
#[derive(Debug, Error)]
pub enum RemoteHelperError {
    #[error("git-remote-slipfork transport failed: {0}")]
    Transport(#[from] std::io::Error),
    #[error("git-remote-slipfork rejected a frame longer than {limit} bytes")]
    FrameTooLarge { limit: usize },
    #[error("git-remote-slipfork received a malformed request: {message}")]
    Malformed { message: String },
    #[error(transparent)]
    Workspace(#[from] VcsError),
}

/// Serve one `git-remote-slipfork` session until Git terminates the command stream.
///
/// `input` carries Git's command lines, `output` carries the helper's responses; both are injected
/// so the handler is testable without a process, a pty, or a filesystem.
pub fn serve(
    workspace: &dyn VcsWorkspace,
    input: impl BufRead,
    output: impl Write,
) -> Result<(), RemoteHelperError> {
    let mut input = input;
    let mut output = output;
    let mut session = Session {
        refs: workspace.refs(),
        workspace,
        output: &mut output,
    };
    session.run(&mut input)
}

/// One helper session: the injected workspace, its advertised refs, and the response stream.
struct Session<'a> {
    workspace: &'a dyn VcsWorkspace,
    refs: WorkspaceRefs,
    output: &'a mut dyn Write,
}

impl Session<'_> {
    fn run(&mut self, input: &mut impl BufRead) -> Result<(), RemoteHelperError> {
        while let Some(frame) = read_frame(input)? {
            if frame.is_empty() {
                // Git terminates its command stream with a blank line.
                return Ok(());
            }
            let (verb, rest) = frame.split_once(' ').unwrap_or((frame.as_str(), ""));
            match verb {
                "capabilities" => self.capabilities()?,
                "option" => self.option(rest)?,
                "list" => self.list()?,
                "fetch" => self.fetch_batch(rest, input)?,
                "push" => self.push_batch(rest, input)?,
                // An unknown verb never panics and never kills the session: the caller keeps the
                // helper alive for the rest of the command stream.
                _ => self.line("unsupported")?,
            }
        }
        Ok(())
    }

    fn capabilities(&mut self) -> Result<(), RemoteHelperError> {
        let mut response = String::new();
        for capability in CAPABILITIES {
            let _ = writeln!(response, "{capability}");
        }
        response.push('\n');
        self.write(&response)
    }

    fn option(&mut self, rest: &str) -> Result<(), RemoteHelperError> {
        let Some((name, value)) = rest.split_once(' ') else {
            return Err(RemoteHelperError::Malformed {
                message: format!("option command without a value: '{rest}'"),
            });
        };
        if !SUPPORTED_OPTIONS.contains(&name) {
            return self.line("unsupported");
        }
        let honoured = match name {
            "progress" => matches!(value, "true" | "false"),
            _ => matches!(value, "0" | "1" | "2"),
        };
        if honoured {
            return self.line("ok");
        }
        self.line(&format!("error unsupported {name} value '{value}'"))
    }

    fn list(&mut self) -> Result<(), RemoteHelperError> {
        let base = self.workspace.resolve(&self.refs.base)?;
        let wip = self.workspace.resolve(&self.refs.wip)?;
        let mut response = String::new();
        let _ = writeln!(response, "{base} {}", self.refs.base);
        let _ = writeln!(response, "{wip} {}", self.refs.wip);
        response.push('\n');
        self.write(&response)
    }

    fn fetch_batch(
        &mut self,
        first: &str,
        input: &mut impl BufRead,
    ) -> Result<(), RemoteHelperError> {
        let mut requested = Vec::new();
        if !first.is_empty() {
            requested.push(parse_fetch(first)?);
        }
        loop {
            let Some(frame) = read_frame(input)? else {
                return Err(RemoteHelperError::Malformed {
                    message: "fetch batch was not terminated by a blank line".to_owned(),
                });
            };
            if frame.is_empty() {
                break;
            }
            let rest =
                frame
                    .strip_prefix("fetch ")
                    .ok_or_else(|| RemoteHelperError::Malformed {
                        message: format!("unexpected '{frame}' line inside a fetch batch"),
                    })?;
            requested.push(parse_fetch(rest)?);
        }
        for (sha, reference) in requested {
            self.verify(&sha, &reference)?;
        }
        // Every advertised object already lives in the caller's object database, so the batch is
        // complete once the advertised ids are verified.
        self.line("")
    }

    fn push_batch(
        &mut self,
        first: &str,
        input: &mut impl BufRead,
    ) -> Result<(), RemoteHelperError> {
        let mut destinations = Vec::new();
        if !first.is_empty() {
            destinations.push(push_destination(first)?);
        }
        loop {
            let Some(frame) = read_frame(input)? else {
                return Err(RemoteHelperError::Malformed {
                    message: "push batch was not terminated by a blank line".to_owned(),
                });
            };
            if frame.is_empty() {
                break;
            }
            let (verb, rest) = frame.split_once(' ').unwrap_or((frame.as_str(), ""));
            match verb {
                "push" => destinations.push(push_destination(rest)?),
                "option" => self.option(rest)?,
                other => {
                    return Err(RemoteHelperError::Malformed {
                        message: format!("unexpected '{other}' line inside a push batch"),
                    });
                }
            }
        }
        let mut response = String::new();
        for destination in destinations {
            let _ = writeln!(response, "error {destination} {PUSH_REFUSAL}");
        }
        response.push('\n');
        self.write(&response)
    }

    /// Fail closed unless `sha` is the id this workspace advertised for `reference`.
    fn verify(&self, sha: &str, reference: &str) -> Result<(), RemoteHelperError> {
        if reference != self.refs.base && reference != self.refs.wip {
            return Err(RemoteHelperError::Malformed {
                message: format!("fetch requested the unadvertised ref '{reference}'"),
            });
        }
        let resolved = self.workspace.resolve(reference)?;
        if resolved != sha {
            return Err(RemoteHelperError::Malformed {
                message: format!(
                    "fetch requested '{reference}' at {sha} but it resolves to {resolved}"
                ),
            });
        }
        Ok(())
    }

    fn line(&mut self, line: &str) -> Result<(), RemoteHelperError> {
        let mut response = String::with_capacity(line.len() + 1);
        response.push_str(line);
        response.push('\n');
        self.write(&response)
    }

    fn write(&mut self, response: &str) -> Result<(), RemoteHelperError> {
        self.output.write_all(response.as_bytes())?;
        self.output.flush()?;
        Ok(())
    }
}

/// Split one `fetch <sha> <refname>` payload into its two (still unverified) halves.
fn parse_fetch(rest: &str) -> Result<(String, String), RemoteHelperError> {
    let Some((sha, reference)) = rest.split_once(' ') else {
        return Err(RemoteHelperError::Malformed {
            message: format!("fetch command misses its refname: '{rest}'"),
        });
    };
    if sha.is_empty() || reference.is_empty() {
        return Err(RemoteHelperError::Malformed {
            message: format!("fetch command has an empty argument: '{rest}'"),
        });
    }
    Ok((sha.to_owned(), reference.to_owned()))
}

/// Destination refname of one `push [+]<src>:<dst>` command (`<src>` when no `:` is present).
fn push_destination(spec: &str) -> Result<String, RemoteHelperError> {
    let spec = spec.strip_prefix('+').unwrap_or(spec);
    let destination = spec
        .split_once(':')
        .map_or(spec, |(_, destination)| destination);
    if destination.is_empty() {
        return Err(RemoteHelperError::Malformed {
            message: format!("push command has no destination: '{spec}'"),
        });
    }
    Ok(destination.to_owned())
}

/// Read one protocol frame, failing closed on oversized or non-UTF-8 input.
fn read_frame(input: &mut impl BufRead) -> Result<Option<String>, RemoteHelperError> {
    let mut buffer = Vec::new();
    let read = (&mut *input)
        .take((MAX_FRAME_BYTES + 1) as u64)
        .read_until(b'\n', &mut buffer)?;
    if read == 0 {
        return Ok(None);
    }
    if read > MAX_FRAME_BYTES {
        return Err(RemoteHelperError::FrameTooLarge {
            limit: MAX_FRAME_BYTES,
        });
    }
    if buffer.last() == Some(&b'\n') {
        buffer.pop();
        if buffer.last() == Some(&b'\r') {
            buffer.pop();
        }
    }
    String::from_utf8(buffer)
        .map(Some)
        .map_err(|err| RemoteHelperError::Malformed {
            message: format!("frame is not valid UTF-8: {err}"),
        })
}
