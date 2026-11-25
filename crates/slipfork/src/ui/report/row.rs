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
    /// A Dockerfile instruction: `[i/n]` with a lowercase padded keyword.
    ///
    /// Carries the plan's instruction width so the keyword column latches once from
    /// `planned_instruction_width` and never grows mid-stream.
    Step {
        /// The planned keyword column width in visible characters.
        instruction_width: usize,
    },
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
    /// A world or service still coming up.
    Starting,
    /// A step that settled without a more specific verb.
    Settled,
    /// One inner step of a live build.
    Step,
    /// A world or fork that came back up.
    Restarted,
    /// A world, fork, or service that stopped.
    Stopped,
    /// A lease, tag, or effect key matched; rendered with a dim bullet.
    Declined,
    /// A neutral settled end with no more specific verb (`finished`).
    Finished,
    /// A mixed image: some instructions were cached and some really built.
    Partial {
        /// Instructions reused from cache; never zero.
        cached: NonZeroUsize,
        /// Instructions really executed; never zero.
        built: NonZeroUsize,
    },
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

    /// The unstyled state cell this completion renders.
    #[must_use]
    pub fn text(&self) -> String {
        match self {
            Self::Built { steps } => {
                let noun = if steps.get() == 1 { "step" } else { "steps" };
                format!("built {steps} {noun}")
            }
            Self::Pulled => "pulled".to_owned(),
            Self::Ready => "ready".to_owned(),
            Self::Running => "running".to_owned(),
            Self::Starting => "starting".to_owned(),
            Self::Settled => "done".to_owned(),
            Self::Step => String::new(),
            Self::Restarted => "restarted".to_owned(),
            Self::Stopped => "stopped".to_owned(),
            Self::Declined => "up to date".to_owned(),
            Self::Finished => "finished".to_owned(),
            Self::Partial { cached, built } => format!("cached {cached} · built {built}"),
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
            Self::Ok(Done::Declined) => palette.meta("•"),
            Self::Ok(Done::Finished) => palette.icon_info(),
            Self::Ok(_) | Self::Cached => palette.tint("✓", "32"),
            Self::Skipped => palette.meta("•"),
            Self::Running => "→".to_owned(),
            Self::Failed { .. } | Self::Refused => palette.error("✗"),
            Self::Problem => palette.error("!"),
            Self::Stopped => palette.meta("○"),
            Self::Orphaned => palette.tint("•", "33"),
        }
    }

    /// One daemon state word (trimmed, case-insensitive) projected onto the closed vocabulary.
    ///
    /// The single authority for every surface that turns a wire `state` into a typed row; an
    /// unknown word yields `None` so callers can fail closed instead of inventing `done`.
    #[must_use]
    pub fn from_state(state: &str) -> Option<Self> {
        match state.trim().to_ascii_lowercase().as_str() {
            "" | "done" => Some(Self::Ok(Done::Settled)),
            "cached" => Some(Self::Cached),
            "pulled" => Some(Self::Ok(Done::Pulled)),
            "ready" => Some(Self::Ok(Done::Ready)),
            "running" => Some(Self::Ok(Done::Running)),
            "starting" => Some(Self::Ok(Done::Starting)),
            "restarted" => Some(Self::Ok(Done::Restarted)),
            "stopped" => Some(Self::Ok(Done::Stopped)),
            "declined" | "up to date" | "up-to-date" => Some(Self::Ok(Done::Declined)),
            "finished" => Some(Self::Ok(Done::Finished)),
            "failed" => Some(Self::Failed { at: None }),
            _ => None,
        }
    }

    /// The state words, already styled; empty only for [`Done::Step`] and [`Self::Problem`].
    #[must_use]
    pub fn state(&self, palette: Palette) -> String {
        match self {
            Self::Problem | Self::Ok(Done::Step) => String::new(),
            Self::Failed { at: Some(_) } | Self::Refused => palette.error(&self.plain()),
            _ => palette.status(&self.plain()),
        }
    }

    /// The same state words with zero escapes, for the column-aligned outer boot row.
    #[must_use]
    pub fn plain(&self) -> String {
        match self {
            Self::Ok(done) => done.text(),
            Self::Cached => "cached".to_owned(),
            Self::Skipped => "up to date".to_owned(),
            Self::Running => "running".to_owned(),
            Self::Failed { at: None } => "failed".to_owned(),
            Self::Failed { at: Some(counter) } => {
                format!("failed at {}/{}", counter.index, counter.total)
            }
            Self::Refused => "refused".to_owned(),
            Self::Problem => String::new(),
            Self::Stopped => "stopped".to_owned(),
            Self::Orphaned => "orphaned".to_owned(),
        }
    }

    /// Whether a row in this state may print its `#run_id`.
    #[must_use]
    pub const fn shows_run_id(&self) -> bool {
        !matches!(
            self,
            Self::Cached | Self::Skipped | Self::Ok(Done::Declined | Done::Finished)
        )
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
    /// One settled outer boot row: `[i/n] keyword  target  icon state  timing`.
    #[must_use]
    pub fn settled(
        counter: Option<Counter>,
        keyword: &str,
        target: &str,
        state: Outcome,
        timing_ms: Option<u64>,
    ) -> Self {
        Self {
            level: RowLevel::Outer,
            counter,
            keyword: Some(keyword.to_owned()),
            target: Some(target.to_owned()),
            state,
            command: None,
            timing_ms,
            id: None,
        }
    }

    /// Carries a copyable command in the row's command column.
    #[must_use]
    pub fn with_command(mut self, command: Option<String>) -> Self {
        self.command = command;
        self
    }

    /// Renders one row: indent, counter, keyword, target, `icon state`, command, timing, `#id`.
    #[must_use]
    pub fn line(&self, palette: Palette) -> String {
        let step_width = match self.level {
            RowLevel::Step { instruction_width } => Some(instruction_width),
            _ => None,
        };
        let indent = match self.level {
            RowLevel::Outer | RowLevel::Step { .. } => 0,
            RowLevel::Inner | RowLevel::Tree => 2,
        };
        let mut fields: Vec<String> = Vec::new();
        if let Some(counter) = self.counter {
            fields.push(palette.meta(&match self.level {
                RowLevel::Outer | RowLevel::Step { .. } => {
                    format!("[{}/{}]", counter.index, counter.total)
                }
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
                RowLevel::Inner | RowLevel::Tree => palette.substep(&keyword.to_ascii_lowercase()),
                RowLevel::Step { instruction_width } => {
                    let keyword = keyword.to_ascii_lowercase();
                    let pad = instruction_width.saturating_sub(keyword.chars().count());
                    format!("{}{}", palette.substep(&keyword), " ".repeat(pad))
                }
            });
        }
        if let Some(target) = &self.target {
            fields.push(palette.value(target));
        }
        // A Dockerfile step's icon is the whole state cell; its keyword/timing already read as
        // the instruction, so `done`/`failed` never repeat beside the mark.
        let state = if step_width.is_some() {
            String::new()
        } else {
            self.state.state(palette)
        };
        let mark = self.state.icon(palette);
        fields.push(if state.is_empty() {
            mark
        } else {
            format!("{mark} {state}")
        });
        if step_width.is_some() {
            if let Some(timing) = self.timing_ms {
                fields.push(palette.meta(&format!("({})", elapsed_text(timing))));
            }
            if let Some(command) = &self.command {
                fields.push(palette.meta(command));
            }
        } else {
            if let Some(command) = &self.command {
                fields.push(command.clone());
            }
            if let Some(timing) = self.timing_ms {
                fields.push(palette.meta(&format!("({})", elapsed_text(timing))));
            }
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

impl FailureFacts {
    /// The typed facts a failed frame carries: its `failure` envelope when present, else the
    /// frame's flat fields, so one `failed` frame always projects the same lines.
    #[must_use]
    pub fn from_event(event: &serde_json::Value, at: Option<Counter>) -> Self {
        if let Some(facts) = event
            .get("failure")
            .and_then(|value| serde_json::from_value(value.clone()).ok())
        {
            return facts;
        }
        Self {
            code: event["code"].as_str().unwrap_or("boot-failed").to_owned(),
            kind: event["kind"].as_str().unwrap_or("boot").to_owned(),
            phase: event["phase"]
                .as_str()
                .filter(|text| !text.is_empty())
                .map(str::to_owned),
            target: event["target"]
                .as_str()
                .or_else(|| event["name"].as_str())
                .map(str::to_owned),
            at,
            exit_code: event["exit_code"]
                .as_i64()
                .and_then(|code| i32::try_from(code).ok()),
            run_id: event["run_id"].as_str().map(str::to_owned),
            reason: event["reason"]
                .as_str()
                .filter(|text| !text.is_empty())
                .unwrap_or_default()
                .to_owned(),
            runtime_ms: event["runtime_ms"]
                .as_u64()
                .or_else(|| event["elapsed_ms"].as_u64()),
            next: event["next"]
                .as_str()
                .map(str::to_owned)
                .unwrap_or_else(|| {
                    event["run_id"]
                        .as_str()
                        .map(|id| format!("slipfork logs -r {id}"))
                        .or_else(|| event["remedy"].as_str().map(str::to_owned))
                        .unwrap_or_else(|| "run `slipfork logs` for the failing step".to_owned())
                }),
        }
    }
}

#[cfg(test)]
mod tests;
