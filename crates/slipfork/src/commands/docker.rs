//! `slipfork docker`: registry credentials, cached OCI images, pulls and native builds.
//!
//! This is a Docker-shaped surface, never a Docker daemon: no `dockerd`, no socket, no second
//! build pipeline. Credentials stay in the owner-only store and every image path reuses the
//! injected `OciPuller`/`NativeImageBuilder` that `up` already boots.

use std::process::ExitCode;

use crate::commands::Session;

mod build;
mod registry;

/// Which `docker` subcommand to run.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DockerVerb {
    /// Verify one host's credentials, then persist them.
    Login {
        /// Registry host, already defaulted to `docker.io` by clap.
        host: String,
        /// Registry username.
        username: String,
        /// Read one password line from stdin instead of prompting on a terminal.
        password_stdin: bool,
    },
    /// Remove one host's stored credentials.
    Logout {
        /// Registry host, already defaulted to `docker.io` by clap.
        host: String,
    },
    /// Show the hosts the Slipfork store owns.
    List,
    /// Show cached OCI references and digests.
    Images,
    /// Pull one OCI reference into the sealed cache.
    Pull {
        /// Reference exactly as typed.
        reference: String,
    },
    /// Build one declared image component up to an optional stage.
    Build {
        /// Component id declared in `./Forkfile`.
        component: String,
        /// Stage that wins over the Forkfile's `stage` declaration.
        target: Option<String>,
    },
}

/// Parsed `slipfork docker` arguments.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DockerArgs {
    /// The chosen subcommand.
    pub verb: DockerVerb,
    /// Emit objects/arrays/NDJSON with zero ANSI.
    pub json: bool,
    /// Show engine narration and per-step detail.
    pub verbose: bool,
}

impl Session {
    /// Runs one credential, cache, pull, or build verb.
    pub fn docker(&self, args: &DockerArgs) -> ExitCode {
        match &args.verb {
            DockerVerb::Login { .. } => self.docker_login(args),
            DockerVerb::Logout { .. } => self.docker_logout(args),
            DockerVerb::List => self.docker_list(args),
            DockerVerb::Images => self.docker_images(args),
            DockerVerb::Pull { .. } => self.docker_pull(args),
            DockerVerb::Build { .. } => self.docker_build(args),
        }
    }
}
