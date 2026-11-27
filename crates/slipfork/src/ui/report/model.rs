//! The typed `Report` vocabulary every CLI surface shares.
//!
//! A [`Report`] is sections of [`Entry`] items plus one optional [`Verdict`]. No field stores a
//! free-form state word: [`super::Outcome`] owns the icon and the state styling, so colour and
//! spelling cannot drift between commands.

use serde::Serialize;

use super::failure::Failure;
use super::row::Row;

/// One CLI result: ordered sections, an optional verdict, and an optional remedy.
#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub struct Report {
    /// Ordered report sections; empty sections render nothing.
    pub sections: Vec<Section>,
    /// The one verdict line or failure block that closes the report.
    pub verdict: Option<Verdict>,
    /// A remedy carried beside a successful report.
    pub remedy: Option<String>,
}

impl Report {
    /// An empty report.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// One report whose verdict is a failed [`Failure`].
    #[must_use]
    pub fn failure(failure: Failure) -> Self {
        Self {
            sections: Vec::new(),
            verdict: Some(Verdict::Failed(failure)),
            remedy: None,
        }
    }

    /// One report carrying a single stdout notice line.
    #[must_use]
    pub fn notice(text: impl Into<String>) -> Self {
        Self {
            sections: vec![Section {
                header: None,
                entries: vec![Entry::Notice {
                    stream: StreamKind::Stdout,
                    text: text.into(),
                }],
            }],
            verdict: None,
            remedy: None,
        }
    }
}

/// One optional-header group of entries.
#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub struct Section {
    /// A bold section label, when the group has one.
    pub header: Option<String>,
    /// The ordered entries below the header.
    pub entries: Vec<Entry>,
}

/// One line or stream chunk inside a [`Section`].
#[derive(Debug, Clone, PartialEq, Serialize)]
pub enum Entry {
    /// A structured status row.
    Row(Row),
    /// One captured child line behind an indented stream gutter.
    Stream {
        /// Leading spaces before the gutter.
        indent: usize,
        /// Which wired stream produced the line.
        stream: StreamKind,
        /// The line itself, already escape-free.
        line: String,
        /// Epoch milliseconds; `0` means the line was not recorded.
        ts_ms: u64,
    },
    /// A verbatim notice written to one stream.
    Notice {
        /// The stream the notice belongs to.
        stream: StreamKind,
        /// The notice text.
        text: String,
    },
    /// A `-v`-only note rendered plainly.
    Note(String),
}

/// Which wired stream an entry belongs to.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub enum StreamKind {
    /// The child's stdout.
    Stdout,
    /// The child's stderr.
    Stderr,
}

impl StreamKind {
    /// The palette's gutter key for this stream.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::Stdout => "stdout",
            Self::Stderr => "stderr",
        }
    }
}

/// The one line that closes a report.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub enum Verdict {
    /// A successful settlement.
    Ok(VerdictKind),
    /// Started work failed.
    Failed(Failure),
    /// Rejected before execution.
    Refused(Failure),
}

/// The closed set of successful verdict words.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub enum VerdictKind {
    /// The world is ready.
    Ready,
    /// A fork was created.
    Forked,
    /// A world restarted.
    Restarted,
    /// A world stopped.
    Stopped,
    /// A world or fork was dropped.
    Dropped,
    /// An image finished building.
    Built,
    /// An image finished pulling.
    Pulled,
    /// A value was stored.
    Set,
    /// A value was removed.
    Unset,
    /// Credentials were stored.
    LoggedIn,
}

impl VerdictKind {
    /// The green verdict word.
    #[must_use]
    pub const fn word(self) -> &'static str {
        match self {
            Self::Ready => "ready",
            Self::Forked => "forked",
            Self::Restarted => "restarted",
            Self::Stopped => "stopped",
            Self::Dropped => "dropped",
            Self::Built => "built",
            Self::Pulled => "pulled",
            Self::Set => "set",
            Self::Unset => "unset",
            Self::LoggedIn => "logged in",
        }
    }
}

/// The one digest cell: an optional `sha256:` prefix plus at most twelve hex characters.
///
/// A short id stays whole, a full digest is shortened once, and a table latches the resulting
/// width from the complete row set — never a hardcoded field.
#[must_use]
pub fn digest_cell(text: &str) -> String {
    let (prefix, body) = match text.strip_prefix("sha256:") {
        Some(hex) => ("sha256:", hex),
        None => ("", text),
    };
    let short: String = body.chars().take(12).collect();
    format!("{prefix}{short}")
}
