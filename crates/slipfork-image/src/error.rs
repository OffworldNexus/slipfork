use std::fmt;
use std::io;
use std::path::{Path, PathBuf};

/// Result alias for every fallible image operation.
pub type Result<T, E = Error> = std::result::Result<T, E>;

/// Everything an OCI pull can refuse to do.
#[derive(Debug)]
pub enum Error {
    /// The caller supplied a reference the OCI grammar cannot express.
    InvalidReference {
        reference: String,
        reason: &'static str,
    },
    /// A credential store entry named no registry host at all.
    InvalidRegistry { registry: String },
    /// A registry login probe was refused for the injected credentials.
    LoginRejected { registry: String, detail: String },
    /// The registry refused the request after authentication.
    Unauthorized {
        registry: String,
        repository: String,
    },
    /// `WWW-Authenticate` was missing or not a bearer challenge.
    Challenge { header: String },
    /// The token endpoint answered without a token.
    Token { realm: String },
    /// A registry response used an unexpected HTTP status.
    Http { url: String, status: u16 },
    /// The network request never produced a response.
    Transport { url: String, message: String },
    /// A manifest or token document was not valid JSON.
    Json {
        url: String,
        source: serde_json::Error,
    },
    /// A manifest list has no entry for the requested platform.
    NoMatchingPlatform { wanted: String, available: String },
    /// A manifest list pointed at another manifest list.
    NestedIndex { url: String },
    /// A downloaded manifest or layer does not match its advertised digest.
    DigestMismatch {
        subject: String,
        expected: String,
        actual: String,
    },
    /// The layer media type has no known decompressor.
    UnsupportedLayer { media_type: String },
    /// A tar entry tried to leave the image root.
    UnsafePath { entry: String },
    /// A tar symlink or hardlink tried to leave the image root.
    UnsafeLink { entry: String, target: String },
    /// A tar entry kind the rootfs unpacker cannot materialize.
    UnsupportedEntry { entry: String, kind: String },
    /// A layer descriptor carried no media type.
    MissingMediaType { digest: String },
    /// A local filesystem operation failed.
    Io {
        action: &'static str,
        path: PathBuf,
        source: io::Error,
    },
    /// The `Dockerfile` could not be parsed at `line`.
    Dockerfile {
        line: usize,
        message: String,
        next: &'static str,
    },
    /// A build instruction failed at `line` (bad source path, failed `RUN`, …).
    StepFailed {
        line: usize,
        /// Instruction keyword (`run`, `copy`, `add`, …).
        phase: &'static str,
        /// The command exactly as the author wrote it, never re-wrapped.
        instruction: Box<str>,
        /// One-line human cause, without container-plumbing detail.
        message: String,
        /// Child process exit code when the failure carried one.
        exit_code: Option<i32>,
        /// Container-runtime plumbing (`crun exited with code 3`); shown only under `-v`.
        runtime: Option<Box<str>>,
        next: String,
    },
    /// A `--from` stage or `stage <name>` target does not exist.
    UnknownStage { name: String },
    /// A `USER`/`COPY --chown` named an account the step's rootfs never created.
    UnknownAccount {
        /// The account name exactly as the `Dockerfile` wrote it.
        account: String,
        /// The image file the lookup searched (`passwd` or `group`).
        file: &'static str,
    },
    /// A `COPY --from=<image>` reference has no pulled rootfs to read.
    ExternalImageMissing { reference: String },
    /// A `FROM` tag moved between planning and unpacking; the planned digest no longer matches.
    MovedTag {
        /// The reference exactly as the `Dockerfile` wrote it.
        reference: String,
        /// The manifest digest the build plan sealed into the step key.
        expected: String,
        /// The digest the tag resolves to now.
        actual: String,
    },
    /// `crun` is not installed, so no `RUN` instruction can execute.
    CrunMissing { path: PathBuf },
    /// An in-process image collaborator was poisoned or panicked.
    Interrupted { message: String },
    /// The Btrfs engine refused a subvolume operation.
    World(slipfork_world::Error),
}

impl Error {
    pub(crate) fn io(action: &'static str, path: &Path, source: io::Error) -> Self {
        Self::Io {
            action,
            path: path.to_path_buf(),
            source,
        }
    }

    /// The child exit code a failed instruction carried, when it carried one.
    #[must_use]
    pub fn exit_code(&self) -> Option<i32> {
        match self {
            Self::StepFailed { exit_code, .. } => *exit_code,
            _ => None,
        }
    }

    /// The `Dockerfile` line a failure names, when it names one.
    #[must_use]
    pub fn line(&self) -> Option<usize> {
        match self {
            Self::StepFailed { line, .. } | Self::Dockerfile { line, .. } => Some(*line),
            _ => None,
        }
    }

    /// The container-runtime plumbing behind a failed `RUN`; `up -v` shows it, the default view
    /// stays a single human line.
    #[must_use]
    pub fn runtime(&self) -> Option<&str> {
        match self {
            Self::StepFailed { runtime, .. } => runtime.as_deref(),
            _ => None,
        }
    }

    /// The engine's raw one-line cause, composed from the variant's own fields.
    ///
    /// No `Dockerfile:<n>:` or `\`<instruction>\` failed:` wrapper, no `next:` remedy, and no
    /// container plumbing. A step the engine executed has no child process to print its own error,
    /// so this line *is* that step's stderr output.
    #[must_use]
    pub fn cause(&self) -> String {
        match self {
            Self::InvalidReference { reference, reason } => {
                format!("`{reference}` is not a valid image reference ({reason})")
            }
            Self::InvalidRegistry { registry } => {
                format!("`{registry}` is not a registry host")
            }
            Self::LoginRejected { registry, detail } => {
                format!("registry `{registry}` rejected the login: {detail}")
            }
            Self::Unauthorized {
                registry,
                repository,
            } => format!("registry `{registry}` rejected the pull of `{repository}`"),
            Self::Challenge { header } => {
                format!("registry sent an unusable auth challenge `{header}`")
            }
            Self::Token { realm } => format!("no bearer token in the response from `{realm}`"),
            Self::Http { url, status } => format!("`{url}` returned HTTP {status}"),
            Self::Transport { url, message } => format!("could not reach `{url}`: {message}"),
            Self::Json { url, .. } if is_remote_url(url) => {
                format!("`{url}` did not return a manifest or token document")
            }
            Self::Json { url, .. } => format!("`{url}` is not valid JSON"),
            Self::NoMatchingPlatform { wanted, available } => {
                format!("image has no `{wanted}` manifest (available: {available})")
            }
            Self::NestedIndex { url } => format!("`{url}` is a nested manifest list"),
            Self::DigestMismatch {
                subject,
                expected,
                actual,
            } => format!("`{subject}` digest mismatch: expected `{expected}`, got `{actual}`"),
            Self::UnsupportedLayer { media_type } => {
                format!("unsupported layer media type `{media_type}`")
            }
            Self::UnsafePath { entry } => format!("layer entry `{entry}` escapes the image root"),
            Self::UnsafeLink { entry, target } => {
                format!("layer link `{entry}` -> `{target}` escapes the image root")
            }
            Self::UnsupportedEntry { entry, kind } => {
                format!("layer entry `{entry}` is a {kind}, which a rootfs cannot carry")
            }
            Self::MissingMediaType { digest } => format!("layer `{digest}` has no media type"),
            Self::Io { action, path, .. } => format!("could not {action} `{}`", path.display()),
            Self::Dockerfile { line, message, .. } => format!("Dockerfile:{line}: {message}"),
            Self::StepFailed { message, .. } => message.clone(),
            Self::UnknownStage { name } => format!("no build stage named `{name}`"),
            Self::UnknownAccount { account, file } => {
                format!("the image has no account `{account}` in `/etc/{file}`")
            }
            Self::ExternalImageMissing { reference } => {
                format!("no rootfs was pulled for `--from={reference}`")
            }
            Self::MovedTag {
                reference,
                expected,
                actual,
            } => format!(
                "`{reference}` moved from `{expected}` to `{actual}` after the build plan resolved it"
            ),
            Self::CrunMissing { path } => {
                format!("`RUN` needs `crun`, but `{}` is missing", path.display())
            }
            Self::Interrupted { message } => {
                format!("the image pull was interrupted: {message}")
            }
            Self::World(err) => err.to_string(),
        }
    }

    /// The one-line remedy for a typed failure, when the variant has one.
    #[must_use]
    pub fn remedy(&self) -> Option<String> {
        let remedy = match self {
            Self::InvalidReference { .. } => "use `<registry>/<repository>[:tag|@sha256:…>`, e.g. `postgres:18`".to_owned(),
            Self::InvalidRegistry { .. } => "use a registry host such as `ghcr.io` or `registry-1.docker.io`".to_owned(),
            Self::LoginRejected { .. } => "check the username and secret for that registry, then retry".to_owned(),
            Self::Unauthorized { .. } => "add credentials for that registry or pull a public image".to_owned(),
            Self::Challenge { .. } | Self::Token { .. } => "check the registry credentials and retry".to_owned(),
            Self::Http { status, .. } => http_remedy(*status).to_owned(),
            Self::Transport { .. } => "check the URL and your network connection, then retry".to_owned(),
            Self::NoMatchingPlatform { .. } => "pull an image that publishes this architecture".to_owned(),
            Self::NestedIndex { .. } => "pull a concrete per-architecture digest".to_owned(),
            Self::DigestMismatch { .. } => "retry the pull; tampered or truncated bytes are never cached".to_owned(),
            Self::UnsupportedLayer { .. } => "only tar+gzip and tar+zstd OCI layers are supported".to_owned(),
            Self::UnsafePath { .. } | Self::UnsafeLink { .. } => "refusing a malicious layer; the pull was rolled back".to_owned(),
            Self::UnsupportedEntry { .. } => "rebuild the image without device nodes, fifos, or sparse files".to_owned(),
            Self::MissingMediaType { .. } => "pull from a registry that labels OCI layers".to_owned(),
            Self::Dockerfile { next, .. } => (*next).to_owned(),
            Self::StepFailed { next, .. } => next.clone(),
            Self::UnknownStage { name } => format!(
                "add `FROM <image> AS {name}` above this stage or fix the `--from=` argument"
            ),
            Self::UnknownAccount { .. } => "create it in an earlier `RUN` (e.g. `useradd`) or use a numeric uid:gid".to_owned(),
            Self::ExternalImageMissing { .. } => "declare the stage with `FROM <image> AS <name>` above this `COPY`, or fix the `--from=` reference".to_owned(),
            Self::MovedTag { .. } => "re-run `slipfork up` so the plan seals the tag's current digest".to_owned(),
            Self::CrunMissing { .. } => "install crun from your distribution (`apt install crun`, `dnf install crun`, `pacman -S crun`) and retry".to_owned(),
            Self::Interrupted { .. } => "retry `slipfork up`".to_owned(),
            Self::Json { url, .. } if is_remote_url(url) => {
                "retry the pull; the registry returned a body that is not a manifest or token".to_owned()
            }
            Self::Json { url, .. } => {
                format!("fix the JSON in `{url}` or delete it to pull anonymously")
            }
            Self::Io { .. } | Self::World(_) => return None,
        };
        Some(remedy)
    }
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            // A step failure keeps its source line and the instruction it ran; the cause itself is
            // the raw one-line message the capture publishes.
            Self::StepFailed {
                line, instruction, ..
            } => write!(
                f,
                "Dockerfile:{line}: `{instruction}` failed: {}",
                self.cause()
            )?,
            other => f.write_str(&other.cause())?,
        }
        if let Some(remedy) = self.remedy() {
            write!(f, "\nnext: {remedy}")?;
        }
        Ok(())
    }
}

/// Whether a typed `url` field is a remote endpoint rather than a local store path.
fn is_remote_url(url: &str) -> bool {
    url.starts_with("http://") || url.starts_with("https://")
}

/// Remedy for a bare HTTP status, kept to one scannable line.
fn http_remedy(status: u16) -> &'static str {
    match status {
        401 => "add credentials for that registry",
        404 => "check the repository, tag, or URL path",
        429 => "retry shortly or authenticate",
        500..=599 => "retry the pull",
        _ => "check the reference and retry",
    }
}

impl std::error::Error for Error {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Json { source, .. } => Some(source),
            Self::Io { source, .. } => Some(source),
            Self::World(err) => Some(err),
            _ => None,
        }
    }
}

impl From<slipfork_world::Error> for Error {
    fn from(err: slipfork_world::Error) -> Self {
        Self::World(err)
    }
}
