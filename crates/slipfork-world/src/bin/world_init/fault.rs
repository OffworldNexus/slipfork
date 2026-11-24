//! The init's typed fault, its remedies, and the frame writers shared by every mode.

use std::fmt;
use std::io::{self, Write};

use slipfork_world::control::{self as wire, InitFrame, PROTOCOL_VERSION};

/// Remedy for an unparseable argv.
pub(crate) const REMEDY_ARGS: &str =
    "run pods through `slipfork`; it passes --namespaces, --pods, and --crun explicitly";
/// Remedy for a broken control frame.
pub(crate) const REMEDY_PROTOCOL: &str =
    "re-run with the `slipfork` and `slipfork-world-init` binaries from this build";
/// Remedy for a missing runtime.
pub(crate) const REMEDY_CRUN: &str =
    "install crun (e.g. `apt install crun`) and confirm `crun --version` runs, then retry";
/// Remedy for a host without unprivileged user namespaces.
pub(crate) const REMEDY_USERNS: &str = "install util-linux and check that unprivileged user namespaces are enabled (`sysctl kernel.unprivileged_userns_clone=1`); then confirm `unshare --user --map-current-user -- true` succeeds for this user";
/// Remedy for a non-tmpfs runtime directory.
pub(crate) const REMEDY_RUNTIME: &str =
    "point `$XDG_RUNTIME_DIR` at a writable tmpfs owned by this user (`/run/user/<uid>`)";
/// Remedy for an invalid pod description.
pub(crate) const REMEDY_POD: &str = "use a lowercase pod id and absolute in-container paths";

/// One init failure: a human action line, a concrete remedy, and OS detail kept for `Debug`.
///
/// `Display` is exactly two lines (action + `next:`); the raw `io::Error` lives on `source()`
/// and `Debug` so errno text never reaches the operator without a way forward.
#[derive(Debug)]
pub(crate) struct Fault {
    action: String,
    remedy: &'static str,
    source: Option<Box<dyn std::error::Error + Send + Sync>>,
    timeout: bool,
}

impl Fault {
    pub(crate) fn new(action: impl Into<String>, remedy: &'static str) -> Self {
        Self {
            action: action.into(),
            remedy,
            source: None,
            timeout: false,
        }
    }

    pub(crate) fn cause(
        action: impl Into<String>,
        remedy: &'static str,
        source: impl std::error::Error + Send + Sync + 'static,
    ) -> Self {
        Self {
            action: action.into(),
            remedy,
            source: Some(Box::new(source)),
            timeout: false,
        }
    }

    /// A deadline was missed after the container was already killed and reaped.
    pub(crate) fn timed_out(action: impl Into<String>, remedy: &'static str) -> Self {
        Self {
            action: action.into(),
            remedy,
            source: None,
            timeout: true,
        }
    }

    pub(crate) fn is_timeout(&self) -> bool {
        self.timeout
    }

    /// Single-line form for the NDJSON reply; `Display` adds the remedy on a second line.
    pub(crate) fn summary(&self) -> String {
        match self.source.as_deref() {
            Some(source) => format!("{} failed: {source}", self.action),
            None => format!("{} failed", self.action),
        }
    }
}

impl fmt::Display for Fault {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}\nnext: {}", self.summary(), self.remedy)
    }
}

impl std::error::Error for Fault {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        self.source
            .as_deref()
            .map(|error| error as &(dyn std::error::Error + 'static))
    }
}

/// The `{"stopped": true}` reply shared by `StopAll` and `Shutdown`.
pub(crate) fn stopped_frame(id: String) -> InitFrame {
    InitFrame::Reply {
        v: PROTOCOL_VERSION,
        id,
        ok: Some(serde_json::json!({ "stopped": true })),
        error: None,
    }
}

/// A typed failure frame carrying only a one-line summary.
pub(crate) fn failure_frame(id: String, code: &str, fault: &Fault) -> InitFrame {
    InitFrame::Reply {
        v: PROTOCOL_VERSION,
        id,
        ok: None,
        error: Some(slipfork_world::ControlError {
            code: code.to_owned(),
            message: fault.summary(),
        }),
    }
}

/// Writes one frame to the init's stdout channel.
pub(crate) fn write_frame(frame: &InitFrame, action: &'static str) -> Result<(), Fault> {
    let bytes =
        wire::encode(frame).map_err(|source| Fault::cause(action, REMEDY_PROTOCOL, source))?;
    let mut stdout = io::stdout();
    stdout
        .write_all(&bytes)
        .and_then(|()| stdout.flush())
        .map_err(|source| Fault::cause(action, REMEDY_PROTOCOL, source))
}
