//! The typed rows behind every status line.
//!
//! A [`Row`] has **no** `icon` and **no** `state: String`: both derive strictly from [`Outcome`],
//! so a command cannot print `✓ failed` or a mismatched colour. [`Done::built_or_cached`] is the
//! only producer of the `built`/`cached` distinction.

use std::num::NonZeroUsize;

use serde::Serialize;

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
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct Counter {
    /// The 1-based position.
    pub index: usize,
    /// The number of siblings.
    pub total: usize,
}

/// How executed work finished.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
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
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
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
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub enum StepSettlement {
    /// Executed work succeeded.
    Completed {
        /// The specific completion verb.
        done: Done,
        /// Wall time in milliseconds.
        duration_ms: u64,
        /// The recorded run id.
        run_id: String,
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
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
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
mod tests {
    use std::num::NonZeroUsize;

    use super::{Counter, Done, FailureFacts, Outcome, Row, RowLevel, StepSettlement};
    use crate::ui::Palette;

    fn counter(index: usize, total: usize) -> Option<Counter> {
        Some(Counter { index, total })
    }

    fn red(mark: &str, word: &str) -> (String, String) {
        (
            format!("\u{1b}[31m{mark}\u{1b}[0m"),
            format!("\u{1b}[31m{word}\u{1b}[0m"),
        )
    }

    #[test]
    fn the_plain_icon_state_table_is_byte_exact() {
        let plain = Palette::new(false);
        for (outcome, icon, state) in [
            (
                Outcome::Ok(Done::Built {
                    steps: NonZeroUsize::new(1).unwrap(),
                }),
                "✓",
                "built 1 step",
            ),
            (
                Outcome::Ok(Done::Built {
                    steps: NonZeroUsize::new(3).unwrap(),
                }),
                "✓",
                "built 3 steps",
            ),
            (Outcome::Ok(Done::Pulled), "✓", "pulled"),
            (Outcome::Ok(Done::Ready), "✓", "ready"),
            (Outcome::Ok(Done::Running), "✓", "running"),
            (Outcome::Ok(Done::Settled), "✓", "done"),
            (Outcome::Ok(Done::Step), "✓", ""),
            (Outcome::Cached, "✓", "cached"),
            (Outcome::Skipped, "•", "up to date"),
            (Outcome::Running, "→", "running"),
            (Outcome::Failed { at: None }, "✗", "failed"),
            (Outcome::Failed { at: counter(2, 3) }, "✗", "failed at 2/3"),
            (Outcome::Refused, "✗", "refused"),
            (Outcome::Problem, "!", ""),
            (Outcome::Stopped, "○", "stopped"),
            (Outcome::Orphaned, "•", "orphaned"),
        ] {
            assert_eq!(outcome.icon(plain), icon, "{outcome:?}");
            assert_eq!(outcome.state(plain), state, "{outcome:?}");
        }
    }

    #[test]
    fn the_colored_icon_state_table_uses_the_role_codes() {
        let colored = Palette::new(true);
        let (failed_icon, failed_state) = red("✗", "failed");
        for (outcome, icon, state) in [
            (
                Outcome::Ok(Done::Ready),
                "\u{1b}[32m✓\u{1b}[0m".to_owned(),
                "\u{1b}[32mready\u{1b}[0m".to_owned(),
            ),
            (
                Outcome::Cached,
                "\u{1b}[32m✓\u{1b}[0m".to_owned(),
                "\u{1b}[32mcached\u{1b}[0m".to_owned(),
            ),
            (
                Outcome::Skipped,
                "\u{1b}[38;5;244m•\u{1b}[0m".to_owned(),
                "\u{1b}[32mup to date\u{1b}[0m".to_owned(),
            ),
            (
                Outcome::Running,
                "→".to_owned(),
                "\u{1b}[32mrunning\u{1b}[0m".to_owned(),
            ),
            (
                Outcome::Failed { at: None },
                failed_icon.clone(),
                failed_state.clone(),
            ),
            (Outcome::Refused, failed_icon.clone(), red("x", "refused").1),
            (
                Outcome::Stopped,
                "\u{1b}[38;5;244m○\u{1b}[0m".to_owned(),
                "\u{1b}[33mstopped\u{1b}[0m".to_owned(),
            ),
            (
                Outcome::Orphaned,
                "\u{1b}[33m•\u{1b}[0m".to_owned(),
                "\u{1b}[33morphaned\u{1b}[0m".to_owned(),
            ),
        ] {
            assert_eq!(outcome.icon(colored), icon, "{outcome:?}");
            assert_eq!(outcome.state(colored), state, "{outcome:?}");
        }
        assert_eq!(
            Outcome::Ok(Done::Built {
                steps: NonZeroUsize::new(2).unwrap()
            })
            .state(colored),
            "\u{1b}[32mbuilt\u{1b}[0m 2 steps"
        );
    }

    #[test]
    fn built_or_cached_maps_zero_to_cached_and_n_to_built() {
        assert_eq!(Done::built_or_cached(0), Outcome::Cached);
        assert_eq!(
            Done::built_or_cached(1),
            Outcome::Ok(Done::Built {
                steps: NonZeroUsize::new(1).unwrap()
            })
        );
        assert_eq!(
            Done::built_or_cached(9),
            Outcome::Ok(Done::Built {
                steps: NonZeroUsize::new(9).unwrap()
            })
        );
    }

    #[test]
    fn cached_and_skipped_rows_never_print_a_run_id() {
        let plain = Palette::new(false);
        for state in [Outcome::Cached, Outcome::Skipped] {
            let row = Row {
                level: RowLevel::Outer,
                counter: counter(1, 2),
                keyword: Some("setup".to_owned()),
                target: Some("api".to_owned()),
                state,
                command: Some("uv sync".to_owned()),
                timing_ms: Some(41),
                id: Some("3f9c21".to_owned()),
            };
            assert!(!row.line(plain).contains('#'), "{}", row.line(plain));
            assert!(!state.shows_run_id());
        }
        let executed = Row {
            level: RowLevel::Outer,
            counter: counter(1, 2),
            keyword: Some("setup".to_owned()),
            target: Some("api".to_owned()),
            state: Outcome::Ok(Done::Built {
                steps: NonZeroUsize::new(1).unwrap(),
            }),
            command: Some("uv sync".to_owned()),
            timing_ms: Some(41),
            id: Some("3f9c21".to_owned()),
        };
        let line = executed.line(plain);
        assert!(line.starts_with("[1/2] SETUP api "), "{line}");
        assert!(line.ends_with(" (41ms) #3f9c21"), "{line}");
    }

    #[test]
    fn every_step_settlement_maps_to_its_outcome() {
        let facts = FailureFacts {
            code: "build".to_owned(),
            kind: "RUN".to_owned(),
            phase: Some("build".to_owned()),
            target: Some("api".to_owned()),
            at: counter(2, 3),
            exit_code: Some(1),
            run_id: Some("abc".to_owned()),
            reason: "exit 1".to_owned(),
            runtime_ms: Some(9),
            next: "slipfork logs".to_owned(),
        };
        assert_eq!(
            StepSettlement::Completed {
                done: Done::Pulled,
                duration_ms: 3,
                run_id: "abc".to_owned(),
            }
            .outcome(),
            Outcome::Ok(Done::Pulled)
        );
        assert_eq!(
            StepSettlement::Cached { duration_ms: 3 }.outcome(),
            Outcome::Cached
        );
        assert_eq!(
            StepSettlement::Skipped { duration_ms: 3 }.outcome(),
            Outcome::Skipped
        );
        assert_eq!(
            StepSettlement::Failed(facts.clone()).outcome(),
            Outcome::Failed { at: counter(2, 3) }
        );
    }
}
