//! The typed rows behind every status line.
//!
//! A [`Row`] has **no** `icon` and **no** `state: String`: both derive strictly from [`Outcome`],
//! so a command cannot print `✓ failed` or a mismatched colour. [`Done::built_or_cached`] is the
//! only producer of the `built`/`cached` distinction.

use std::num::NonZeroUsize;

use serde::{Deserialize, Serialize};

use crate::ui::{Palette, elapsed_text};

/// Where one row sits in a two-level plan or a status tree.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub enum RowLevel {
    /// A top-level step: `[i/n]`.
    Outer,
    /// An indented substep: `n/N`, no target.
    Inner,
    /// A status-tree branch.
    Tree,
}

/// One position in a two-level plan.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Counter {
    /// The 1-based position.
    pub index: usize,
    /// The number of siblings.
    pub total: usize,
}

/// How executed work finished.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Done {
    /// `n` executed build steps.
    Built {
        /// Executed step count; never zero.
        steps: NonZeroUsize,
    },
    /// An image pull.
    Pulled,
    /// A world that reached ready.
    Ready,
    /// A process that is running.
    Running,
    /// A step that settled without a more specific verb.
    Settled,
    /// One inner step of a live build.
    Step,
}

impl Done {
    /// Maps an executed-step count to `cached` (`0`) or `built N steps` (`n >= 1`).
    #[must_use]
    pub fn built_or_cached(executed: usize) -> Outcome {
        match NonZeroUsize::new(executed) {
            Some(steps) => Outcome::Ok(Self::Built { steps }),
            None => Outcome::Cached,
        }
    }
}

/// The closed settlement states of one row.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Outcome {
    /// Executed work succeeded.
    Ok(Done),
    /// A cache hit with zero executed steps.
    Cached,
    /// A lease or effect key matched; zero commands ran.
    Skipped,
    /// In flight, not yet settled.
    Running,
    /// Started work failed at an optional inner step.
    Failed {
        /// The inner step that failed, when known.
        at: Option<Counter>,
    },
    /// Rejected before execution.
    Refused,
    /// A problem row whose reason the caller carries in `target`/`command`.
    Problem,
    /// An inactive world or fork.
    Stopped,
    /// A live pod missing from the `Forkfile`.
    Orphaned,
}

impl Outcome {
    /// The mark that prefixes the state.
    #[must_use]
    pub fn icon(&self, palette: Palette) -> String {
        match self {
            Self::Ok(_) | Self::Cached => palette.tint("✓", "32"),
            Self::Skipped => palette.meta("•"),
            Self::Running => "→".to_owned(),
            Self::Failed { .. } | Self::Refused => palette.error("✗"),
            Self::Problem => palette.error("!"),
            Self::Stopped => palette.meta("○"),
            Self::Orphaned => palette.tint("•", "33"),
        }
    }

    /// The state words, already styled; empty only for [`Done::Step`] and [`Self::Problem`].
    #[must_use]
    pub fn state(&self, palette: Palette) -> String {
        match self {
            Self::Ok(done) => match done {
                Done::Built { steps } => {
                    let noun = if steps.get() == 1 { "step" } else { "steps" };
                    format!("{} {steps} {noun}", palette.tint("built", "32"))
                }
                Done::Pulled => palette.tint("pulled", "32"),
                Done::Ready => palette.tint("ready", "32"),
                Done::Running => palette.tint("running", "32"),
                Done::Settled => palette.tint("done", "32"),
                Done::Step => String::new(),
            },
            Self::Cached => palette.tint("cached", "32"),
            Self::Skipped => palette.tint("up to date", "32"),
            Self::Running => palette.tint("running", "32"),
            Self::Failed { at: None } => palette.tint("failed", "31"),
            Self::Failed { at: Some(counter) } => palette.tint(
                &format!("failed at {}/{}", counter.index, counter.total),
                "31",
            ),
            Self::Refused => palette.tint("refused", "31"),
            Self::Problem => String::new(),
            Self::Stopped => palette.tint("stopped", "33"),
            Self::Orphaned => palette.tint("orphaned", "33"),
        }
    }

    /// Whether a row in this state may print its `#run_id`.
    #[must_use]
    pub const fn shows_run_id(&self) -> bool {
        !matches!(self, Self::Cached | Self::Skipped)
    }
}

/// One status row: level, optional counter/keyword/target, a typed state, and optional metadata.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Row {
    /// Where this row sits in the plan or tree.
    pub level: RowLevel,
    /// The `[i/n]` (outer) or `n/N` (inner) position.
    pub counter: Option<Counter>,
    /// The instruction or verb.
    pub keyword: Option<String>,
    /// The thing the row is about.
    pub target: Option<String>,
    /// The settlement that owns the icon and state styling.
    pub state: Outcome,
    /// The command a human can copy.
    pub command: Option<String>,
    /// Wall time in milliseconds.
    pub timing_ms: Option<u64>,
    /// The recorded run id, printed as `#id` when the state allows it.
    pub id: Option<String>,
}

impl Row {
    /// Renders one row: indent, counter, keyword, target, `icon state`, command, timing, `#id`.
    #[must_use]
    pub fn line(&self, palette: Palette) -> String {
        let indent = match self.level {
            RowLevel::Outer => 0,
            RowLevel::Inner | RowLevel::Tree => 2,
        };
        let mut fields: Vec<String> = Vec::new();
        if let Some(counter) = self.counter {
            fields.push(palette.meta(&match self.level {
                RowLevel::Outer => format!("[{}/{}]", counter.index, counter.total),
                RowLevel::Inner | RowLevel::Tree => {
                    format!("{}/{}", counter.index, counter.total)
                }
            }));
        }
        if self.level != RowLevel::Tree
            && let Some(keyword) = &self.keyword
        {
            fields.push(match self.level {
                RowLevel::Outer => keyword.to_ascii_uppercase(),
                RowLevel::Inner | RowLevel::Tree => keyword.to_ascii_lowercase(),
            });
        }
        if let Some(target) = &self.target {
            fields.push(palette.value(target));
        }
        let state = self.state.state(palette);
        let mark = self.state.icon(palette);
        fields.push(if state.is_empty() {
            mark
        } else {
            format!("{mark} {state}")
        });
        if let Some(command) = &self.command {
            fields.push(command.clone());
        }
        if let Some(timing) = self.timing_ms {
            fields.push(palette.meta(&format!("({})", elapsed_text(timing))));
        }
        if let Some(id) = &self.id
            && self.state.shows_run_id()
        {
            fields.push(palette.meta(&format!("#{id}")));
        }
        let body = fields.join(" ");
        if body.is_empty() {
            String::new()
        } else {
            format!("{}{body}", " ".repeat(indent))
        }
    }
}

/// How one step settled before it became a [`Row`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "state")]
pub enum StepSettlement {
    /// Executed work succeeded.
    Completed {
        /// The specific completion verb.
        done: Done,
        /// Wall time in milliseconds.
        duration_ms: u64,
        /// The recorded run id.
        run_id: Option<String>,
    },
    /// A cache hit with zero executed steps.
    Cached {
        /// Wall time in milliseconds.
        duration_ms: u64,
    },
    /// A lease or effect key matched.
    Skipped {
        /// Wall time in milliseconds.
        duration_ms: u64,
    },
    /// Started work failed.
    Failed(FailureFacts),
}

impl StepSettlement {
    /// The row state this settlement maps to.
    #[must_use]
    pub fn outcome(&self) -> Outcome {
        match self {
            Self::Completed { done, .. } => Outcome::Ok(*done),
            Self::Cached { .. } => Outcome::Cached,
            Self::Skipped { .. } => Outcome::Skipped,
            Self::Failed(facts) => Outcome::Failed { at: facts.at },
        }
    }
}

/// Typed failure facts shared by the stderr block and the `--json` error object.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FailureFacts {
    /// The stable machine code.
    pub code: String,
    /// The step or component kind (`image`, `RUN`, `setup`).
    pub kind: String,
    /// The build phase, when the failure happened inside one.
    pub phase: Option<String>,
    /// The thing that failed.
    pub target: Option<String>,
    /// The inner step that failed, when known.
    pub at: Option<Counter>,
    /// The child exit code, when there was one.
    pub exit_code: Option<i32>,
    /// The recorded run id, when one exists.
    pub run_id: Option<String>,
    /// The one-line reason.
    pub reason: String,
    /// Wall time in milliseconds.
    pub runtime_ms: Option<u64>,
    /// The one `next:` remedy.
    pub next: String,
}

#[cfg(test)]
mod tests;
