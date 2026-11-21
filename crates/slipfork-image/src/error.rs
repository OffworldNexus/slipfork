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
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidReference { reference, reason } => write!(
                f,
                "`{reference}` is not a valid image reference ({reason})\nnext: use `<registry>/<repository>[:tag|@sha256:…>`, e.g. `postgres:18`"
            ),
            Self::Unauthorized {
                registry,
                repository,
            } => write!(
                f,
                "registry `{registry}` rejected the pull of `{repository}`\nnext: add credentials for that registry or pull a public image"
            ),
            Self::Challenge { header } => write!(
                f,
                "registry sent an unusable auth challenge `{header}`\nnext: check the registry credentials and retry"
            ),
            Self::Token { realm } => write!(
                f,
                "no bearer token in the response from `{realm}`\nnext: check the registry credentials and retry"
            ),
            Self::Http { url, status } => write!(
                f,
                "`{url}` returned HTTP {status}\nnext: {}",
                http_remedy(*status)
            ),
            Self::Transport { url, message } => {
                write!(f, "could not reach `{url}`: {message}")
            }
            Self::Json { url, .. } => {
                write!(f, "`{url}` did not return a manifest or token document")
            }
            Self::NoMatchingPlatform { wanted, available } => write!(
                f,
                "image has no `{wanted}` manifest (available: {available})\nnext: pull an image that publishes this architecture"
            ),
            Self::NestedIndex { url } => write!(
                f,
                "`{url}` is a nested manifest list\nnext: pull a concrete per-architecture digest"
            ),
            Self::DigestMismatch {
                subject,
                expected,
                actual,
            } => write!(
                f,
                "`{subject}` digest mismatch: expected `{expected}`, got `{actual}`\nnext: retry the pull; tampered or truncated bytes are never cached"
            ),
            Self::UnsupportedLayer { media_type } => write!(
                f,
                "unsupported layer media type `{media_type}`\nnext: only tar+gzip and tar+zstd OCI layers are supported"
            ),
            Self::UnsafePath { entry } => write!(
                f,
                "layer entry `{entry}` escapes the image root\nnext: refusing a malicious layer; the pull was rolled back"
            ),
            Self::UnsafeLink { entry, target } => write!(
                f,
                "layer link `{entry}` -> `{target}` escapes the image root\nnext: refusing a malicious layer; the pull was rolled back"
            ),
            Self::UnsupportedEntry { entry, kind } => write!(
                f,
                "layer entry `{entry}` is a {kind}, which a rootfs cannot carry\nnext: rebuild the image without device nodes, fifos, or sparse files"
            ),
            Self::MissingMediaType { digest } => write!(
                f,
                "layer `{digest}` has no media type\nnext: pull from a registry that labels OCI layers"
            ),
            Self::Io { action, path, .. } => write!(f, "could not {action} `{}`", path.display()),
            Self::World(err) => fmt::Display::fmt(err, f),
        }
    }
}

/// Remedy for a bare HTTP status, kept to one scannable line.
fn http_remedy(status: u16) -> &'static str {
    match status {
        401 => "add credentials for that registry",
        404 => "check the repository name and tag",
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
