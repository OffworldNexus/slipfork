//! Direct-`execve` `Forkfile` lifecycle runner: sequential argv, `[1/N]` progress, and
//! `Forkfile:<line>` fail-fast (`docs/design/forkfile.md` §3, `docs/design/dx-cli-0.1.0.md` §1).
//!
//! The runner owns no world, no process, and no ambient state: the caller injects the executor
//! (a pod/world `exec_in_service` call) and receives a start event plus one completion event per
//! step, so a CLI can render `[1/N]` headers and spinners while a slow step runs. Every command
//! line is resolved through [`slipfork_forkfile::EvaluatedFork::resolve_argv`], so a
//! secret-tainted slot is only ever printed as `[REDACTED]` — no event or error carries argv text.

#![forbid(unsafe_code)]

use std::time::{Duration, Instant};

use slipfork_forkfile::{Argv, Diagnostic, EvaluatedFork, HookPhase, Resolvers};
use slipfork_world::{EngineError, ExecOutput};

/// Output lines echoed beside a failing step, newest last.
const FAILURE_TAIL_LINES: usize = 15;

/// One step: emitted once when the child starts (`ok: None`) and once when it finishes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LifecycleStep {
    /// The service the step runs in.
    pub service: String,
    /// The `Forkfile` lifecycle phase, when the step is a hook (tasks have no phase).
    pub phase: Option<HookPhase>,
    /// The phase spelling (`setup`, `pre-deploy`, …) or the task name.
    pub label: String,
    /// 1-based step number.
    pub index: usize,
    /// Number of steps in this sequence.
    pub total: usize,
    /// Wall time so far (`0` on the start event).
    pub elapsed: Duration,
    /// `None` while the step runs; `Some(ok)` once it exits.
    pub ok: Option<bool>,
}

/// A lifecycle sequence that could not complete.
#[derive(Debug, thiserror::Error)]
pub enum LifecycleError {
    /// The evaluated fork has no such service.
    #[error(
        "Forkfile names no service `{service}`\n  next: services in this Forkfile are {available}"
    )]
    UnknownService { service: String, available: String },
    /// The evaluated fork has no such task.
    #[error(
        "Forkfile names no task `{task}`\n  next: run a declared task with `slipfork run <task>` (tasks: {available})"
    )]
    UnknownTask { task: String, available: String },
    /// A multi-line task block rejects trailing arguments.
    #[error(
        "task `{task}` is a multi-line block; extra arguments are rejected\n  next: drop the extra arguments, or run a single-command task"
    )]
    MultiCommandTask { task: String },
    /// A command line could not be resolved (missing port, env key, or fact).
    #[error("{report}")]
    Resolve { report: String },
    /// The executor refused to start the step.
    #[error("Forkfile:{line} cannot start `{phase}` step {index}/{total}: {message}")]
    Exec {
        line: u32,
        phase: String,
        index: usize,
        total: usize,
        message: String,
    },
    /// The step ran and exited non-zero; the tail is what the developer needs to see.
    #[error("{report}")]
    Failed { report: String },
}

/// Runs a service's lifecycle hooks and named tasks, one direct child at a time.
pub struct ForkfileRunner<'a> {
    fork: &'a EvaluatedFork,
    resolvers: &'a Resolvers<'a>,
}

impl<'a> ForkfileRunner<'a> {
    /// Borrow one evaluated `Forkfile` and the resolvers its `$port`/`$env`/`$<entity>` slots use.
    pub fn new(fork: &'a EvaluatedFork, resolvers: &'a Resolvers<'a>) -> Self {
        Self { fork, resolvers }
    }

    /// Run every `setup`/`pre-deploy`/`post-deploy`/`failed-deploy` line of one service, in
    /// declaration order, stopping at the first failure.
    pub fn run_phase(
        &self,
        service: &str,
        phase: HookPhase,
        executor: &mut dyn FnMut(&[String]) -> Result<ExecOutput, EngineError>,
        on_step: &mut dyn FnMut(&LifecycleStep),
    ) -> Result<(), LifecycleError> {
        let Some(entry) = self.fork.services.iter().find(|entry| entry.id == service) else {
            return Err(LifecycleError::UnknownService {
                service: service.to_owned(),
                available: ids(self.fork.services.iter().map(|entry| entry.id.as_str())),
            });
        };
        let commands: Vec<&Argv> = entry
            .hooks
            .iter()
            .filter(|hook| hook.phase == phase)
            .map(|hook| &hook.argv)
            .collect();
        Sequence {
            fork: self.fork,
            resolvers: self.resolvers,
            service,
            label: phase.as_str().to_owned(),
            phase: Some(phase),
            commands,
            extra: &[],
        }
        .run(executor, on_step)
    }

    /// Run one named task in its service; a single-command task appends `extra` to its argv.
    pub fn run_task(
        &self,
        task: &str,
        extra: &[String],
        executor: &mut dyn FnMut(&[String]) -> Result<ExecOutput, EngineError>,
        on_step: &mut dyn FnMut(&LifecycleStep),
    ) -> Result<(), LifecycleError> {
        let Some(definition) = self.fork.tasks.iter().find(|entry| entry.name == task) else {
            return Err(LifecycleError::UnknownTask {
                task: task.to_owned(),
                available: ids(self.fork.tasks.iter().map(|entry| entry.name.as_str())),
            });
        };
        if definition.multi && !extra.is_empty() {
            return Err(LifecycleError::MultiCommandTask {
                task: task.to_owned(),
            });
        }
        Sequence {
            fork: self.fork,
            resolvers: self.resolvers,
            service: &definition.service,
            label: task.to_owned(),
            phase: None,
            commands: definition.commands.iter().collect(),
            extra,
        }
        .run(executor, on_step)
    }
}

/// One resolved, ordered command sequence (a hook phase or a task).
struct Sequence<'a, 'f> {
    fork: &'f EvaluatedFork,
    resolvers: &'f Resolvers<'f>,
    service: &'a str,
    label: String,
    phase: Option<HookPhase>,
    commands: Vec<&'f Argv>,
    extra: &'a [String],
}

impl Sequence<'_, '_> {
    fn run(
        &self,
        executor: &mut dyn FnMut(&[String]) -> Result<ExecOutput, EngineError>,
        on_step: &mut dyn FnMut(&LifecycleStep),
    ) -> Result<(), LifecycleError> {
        let total = self.commands.len();
        let appends = total == 1 && !self.extra.is_empty();
        for (index, argv) in self.commands.iter().enumerate() {
            let line = argv.span.line;
            let resolved = self
                .fork
                .resolve_argv(self.service, argv, self.resolvers)
                .map_err(|diagnostics| LifecycleError::Resolve {
                    report: resolve_report(self.service, &self.label, line, &diagnostics),
                })?;
            let mut launched: Vec<String> = resolved
                .iter()
                .map(|slot| slot.expose().to_owned())
                .collect();
            if appends {
                launched.extend(self.extra.iter().cloned());
            }
            let step = |elapsed: Duration, ok: Option<bool>| LifecycleStep {
                service: self.service.to_owned(),
                phase: self.phase,
                label: self.label.clone(),
                index: index + 1,
                total,
                elapsed,
                ok,
            };
            // Start event first: `[1/N]` + spinner can render while the child runs.
            on_step(&step(Duration::ZERO, None));
            let started = Instant::now();
            let result = executor(&launched);
            let elapsed = started.elapsed();
            match result {
                Ok(output) if output.exit_code == 0 => on_step(&step(elapsed, Some(true))),
                Ok(output) => {
                    on_step(&step(elapsed, Some(false)));
                    return Err(LifecycleError::Failed {
                        report: failure_report(
                            self.service,
                            &self.label,
                            index + 1,
                            total,
                            argv.span.line,
                            &output,
                        ),
                    });
                }
                Err(error) => {
                    on_step(&step(elapsed, Some(false)));
                    return Err(LifecycleError::Exec {
                        line,
                        phase: self.label.clone(),
                        index: index + 1,
                        total,
                        message: error.to_string(),
                    });
                }
            }
        }
        Ok(())
    }
}

fn ids<'a>(ids: impl Iterator<Item = &'a str>) -> String {
    let collected: Vec<&str> = ids.collect();
    if collected.is_empty() {
        "none".to_owned()
    } else {
        collected.join(", ")
    }
}

fn resolve_report(service: &str, label: &str, line: u32, diagnostics: &[Diagnostic]) -> String {
    let messages: Vec<&str> = diagnostics
        .iter()
        .map(|diagnostic| diagnostic.message.as_str())
        .collect();
    let mut report = format!(
        "Forkfile:{line} cannot resolve `{label}` for `{service}`: {}",
        messages.join("; ")
    );
    let remedies: Vec<&str> = diagnostics
        .iter()
        .filter_map(|diagnostic| diagnostic.hint.as_deref())
        .collect();
    if !remedies.is_empty() {
        report.push_str(&format!("\n  next: {}", remedies.join("; ")));
    }
    report
}

fn failure_report(
    service: &str,
    label: &str,
    index: usize,
    total: usize,
    line: u32,
    output: &ExecOutput,
) -> String {
    let mut report = format!(
        "Forkfile:{line} `{label}` step {index}/{total} failed (exit {})",
        output.exit_code
    );
    let tail = failure_tail(output);
    if !tail.is_empty() {
        report.push('\n');
        report.push_str(&tail);
    }
    report.push_str(&format!(
        "\n  next: slipfork logs {service} | slipfork shell {service}"
    ));
    report
}

/// The last [`FAILURE_TAIL_LINES`] lines of one failed step, stderr first then stdout.
///
/// `ExecOutput` carries the two streams separately (the executor captures them independently), so
/// this preserves each stream's own order and keeps the newest lines of the merged view.
fn failure_tail(output: &ExecOutput) -> String {
    let lines: Vec<&str> = output
        .stderr
        .lines()
        .chain(output.stdout.lines())
        .filter(|line| !line.trim().is_empty())
        .collect();
    lines[lines.len().saturating_sub(FAILURE_TAIL_LINES)..].join("\n")
}

#[cfg(test)]
mod tests {
    use std::process::Command;

    use slipfork_forkfile::{
        EvaluatedFork, Facts, FeatureSet, FileVaultProvider, HookPhase, OsCsprngEntropy, Resolvers,
        parse_forkfile,
    };

    use super::{ForkfileRunner, LifecycleError, LifecycleStep};
    use slipfork_world::{EngineError, ExecOutput};

    fn evaluate(source: &str) -> EvaluatedFork {
        parse_forkfile(source)
            .expect("parse")
            .evaluate(&FeatureSet::new())
            .expect("evaluate")
    }

    fn real_executor(argv: &[String]) -> Result<ExecOutput, EngineError> {
        let output = Command::new(&argv[0])
            .args(&argv[1..])
            .output()
            .map_err(|error| EngineError::Execution(error.to_string()))?;
        Ok(ExecOutput {
            exit_code: output.status.code().unwrap_or(1),
            stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
            stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
        })
    }

    fn with_runner(source: &str, facts: Facts, run: impl FnOnce(&ForkfileRunner<'_>)) {
        let fork = evaluate(source);
        let dir = tempfile::tempdir().expect("disk vault dir");
        let vault = FileVaultProvider::open(dir.path().join("vault.json")).expect("vault");
        let entropy = OsCsprngEntropy;
        let resolvers = Resolvers::new(&vault, &entropy, &facts);
        run(&ForkfileRunner::new(&fork, &resolvers));
    }

    fn hooks(fork: &EvaluatedFork, phase: HookPhase) -> usize {
        fork.services[0]
            .hooks
            .iter()
            .filter(|hook| hook.phase == phase)
            .count()
    }

    #[test]
    fn a_failing_hook_reports_its_line_stops_the_sequence_and_announces_the_start() {
        let source = "features {}\n\
                      service api ./api {\n\
                          setup: /bin/cat /slipfork-does-not-exist\n\
                          setup: /bin/echo must-not-run\n\
                      }\n";
        let fork = evaluate(source);
        assert_eq!(
            hooks(&fork, HookPhase::Setup),
            2,
            "two declared setup lines"
        );
        let dir = tempfile::tempdir().expect("disk vault dir");
        let vault = FileVaultProvider::open(dir.path().join("vault.json")).expect("vault");
        let facts = Facts::default();
        let entropy = OsCsprngEntropy;
        let resolvers = Resolvers::new(&vault, &entropy, &facts);
        let runner = ForkfileRunner::new(&fork, &resolvers);

        let mut ran: Vec<String> = Vec::new();
        let mut steps: Vec<LifecycleStep> = Vec::new();
        let mut execute = |command: &[String]| {
            ran.push(command.join(" "));
            real_executor(command)
        };
        let error = runner
            .run_phase("api", HookPhase::Setup, &mut execute, &mut |step| {
                steps.push(step.clone());
            })
            .expect_err("the first setup line fails");

        assert_eq!(
            ran,
            ["/bin/cat /slipfork-does-not-exist"],
            "the sequence stops at the failing line"
        );
        assert_eq!(
            steps.len(),
            2,
            "start + completion for the only executed step"
        );
        assert_eq!(steps[0].ok, None, "the start event renders `[1/N]` live");
        assert_eq!(steps[0].phase, Some(HookPhase::Setup));
        assert_eq!((steps[0].index, steps[0].total), (1, 2));
        assert_eq!(steps[1].ok, Some(false));
        let message = error.to_string();
        assert!(
            message.contains("Forkfile:3"),
            "cites the source line: {message}"
        );
        assert!(
            message.contains("/slipfork-does-not-exist"),
            "echoes the failing output inline: {message}"
        );
        assert!(
            message.contains("next: slipfork logs api | slipfork shell api"),
            "names the remedy: {message}"
        );
    }

    #[test]
    fn the_failure_tail_keeps_the_last_fifteen_lines_of_both_streams() {
        let mut paths = String::new();
        for index in 1..=16 {
            paths.push_str(&format!(" /nope{index}"));
        }
        let source = format!(
            "features {{}}\nservice api ./api {{\n    setup: /bin/cat{paths} /etc/hostname\n}}\n"
        );
        let fork = evaluate(&source);
        let dir = tempfile::tempdir().expect("disk vault dir");
        let vault = FileVaultProvider::open(dir.path().join("vault.json")).expect("vault");
        let facts = Facts::default();
        let entropy = OsCsprngEntropy;
        let resolvers = Resolvers::new(&vault, &entropy, &facts);
        let runner = ForkfileRunner::new(&fork, &resolvers);
        let mut execute = |command: &[String]| real_executor(command);
        let error = runner
            .run_phase("api", HookPhase::Setup, &mut execute, &mut |_| {})
            .expect_err("`cat` exits non-zero for missing files");
        let report = error.to_string();
        let lines: Vec<&str> = report.lines().collect();
        // header, 15 tail lines, `next:` line.
        assert_eq!(lines.len(), 1 + 15 + 1, "exactly 15 output lines: {report}");
        assert!(report.contains("/nope16"), "newest stderr line survives");
        assert!(!report.contains("/nope1 "), "oldest lines are dropped");
        let hostname = std::fs::read_to_string("/etc/hostname").expect("hostname");
        assert!(
            report.contains(hostname.trim()),
            "the stdout stream is represented: {report}"
        );
    }

    #[test]
    fn a_quoted_space_stays_one_argv_element() {
        let source =
            "features {}\nservice api ./api {\n    setup: /bin/printf [%s] \"hello world\"\n}\n";
        with_runner(source, Facts::default(), |runner| {
            let mut ran: Vec<Vec<String>> = Vec::new();
            let mut execute = |command: &[String]| {
                ran.push(command.to_vec());
                real_executor(command)
            };
            runner
                .run_phase("api", HookPhase::Setup, &mut execute, &mut |_| {})
                .expect("the printf hook succeeds");
            assert_eq!(
                ran,
                [vec![
                    "/bin/printf".to_owned(),
                    "[%s]".to_owned(),
                    "hello world".to_owned()
                ]],
                "the quoted space is one element, never word-split"
            );
        });
    }

    #[test]
    fn a_real_multi_line_task_block_rejects_extra_arguments() {
        let source = "features {}\n\
                      service api ./api { setup: /bin/echo hi }\n\
                      task migrate in api {\n\
                          /bin/echo one\n\
                          /bin/echo two\n\
                      }\n";
        let fork = evaluate(source);
        assert!(fork.tasks[0].multi, "the parser marks a brace block multi");
        assert_eq!(fork.tasks[0].commands.len(), 2);
        let dir = tempfile::tempdir().expect("disk vault dir");
        let vault = FileVaultProvider::open(dir.path().join("vault.json")).expect("vault");
        let facts = Facts::default();
        let entropy = OsCsprngEntropy;
        let resolvers = Resolvers::new(&vault, &entropy, &facts);
        let runner = ForkfileRunner::new(&fork, &resolvers);

        let mut ran: Vec<String> = Vec::new();
        {
            let mut execute = |command: &[String]| {
                ran.push(command.join(" "));
                real_executor(command)
            };
            let error = runner
                .run_task("migrate", &["tail".to_owned()], &mut execute, &mut |_| {})
                .expect_err("a block task rejects trailing arguments");
            assert!(matches!(error, LifecycleError::MultiCommandTask { .. }));
            assert!(error.to_string().contains("next: drop the extra arguments"));
        }
        assert!(ran.is_empty(), "nothing runs when the block rejects extras");

        {
            let mut execute = |command: &[String]| {
                ran.push(command.join(" "));
                real_executor(command)
            };
            runner
                .run_task("migrate", &[], &mut execute, &mut |_| {})
                .expect("the block runs both declared lines in order");
        }
        assert_eq!(ran, ["/bin/echo one", "/bin/echo two"]);
    }

    #[test]
    fn unknown_services_and_tasks_name_what_is_declared() {
        let source = "features {}\nservice api ./api { setup: /bin/echo hi }\n";
        with_runner(source, Facts::default(), |runner| {
            let mut execute = |_command: &[String]| real_executor(&["/bin/true".to_owned()]);
            let error = runner
                .run_phase("nope", HookPhase::Setup, &mut execute, &mut |_| {})
                .expect_err("unknown service");
            let message = error.to_string();
            assert!(message.contains("next: services in this Forkfile are api"));

            let error = runner
                .run_task("nope", &[], &mut execute, &mut |_| {})
                .expect_err("unknown task");
            assert!(error.to_string().contains("slipfork run <task>"));
        });
    }

    #[test]
    fn an_unresolvable_hook_folds_the_diagnostic_remedy() {
        let source = "features {}\n\
                      postgres pg { database app_db }\n\
                      service api ./api { setup: /bin/echo $app_db.url }\n";
        // No facts were injected: the reference is schema-valid but unavailable at runtime.
        with_runner(source, Facts::default(), |runner| {
            let mut execute = |command: &[String]| real_executor(command);
            let error = runner
                .run_phase("api", HookPhase::Setup, &mut execute, &mut |_| {})
                .expect_err("the property is not resolved");
            let message = error.to_string();
            assert!(message.contains("Forkfile:3"), "{message}");
            assert!(
                message.contains(
                    "next: the daemon resolves resource properties before the world starts"
                ),
                "the diagnostic hint becomes the remedy: {message}"
            );
        });
    }
}
